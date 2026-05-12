use anyhow::{anyhow, Context, Result};
use audios_common::{
    config::ClientConfigFile,
    paths,
    types::{ConvertManifest, FetchManifest, MatchItem, MatchManifest},
    util,
};
use chrono::{NaiveDateTime, Utc};
use clap::Parser;
use futures_util::TryStreamExt;
use glob::glob;
use serde::Deserialize;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};
use tiberius::{Client, Config};
use tokio::net::TcpStream;
use tokio_util::compat::TokioAsyncWriteCompatExt;

#[derive(Parser, Debug)]
#[command(name = "metadata-matcher-rs")]
struct Args {
    #[arg(long)]
    client: String,

    #[arg(long)]
    date: String,

    #[arg(long, default_value = "/shared")]
    shared_root: String,

    #[arg(long, default_value = "config/clients")]
    clients_dir: String,

    #[arg(long)]
    run_id: Option<String>,

    #[arg(long)]
    dry_run: bool,

    #[arg(long, default_value = "/run/secrets/mssql-env-v2")]
    mssql_env_file: String,
}

fn read_kv_env_file(path: &Path) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let s = match fs::read_to_string(path) {
        Ok(v) => v,
        Err(_) => return m,
    };
    for line in s.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            m.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    m
}

fn env_get(k: &str, file_env: &HashMap<String, String>) -> Option<String> {
    std::env::var(k).ok().or_else(|| file_env.get(k).cloned())
}

fn parse_bool(s: &str) -> bool {
    matches!(s.trim().to_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

async fn mssql_connect(env_file: &HashMap<String, String>) -> Result<Client<tokio_util::compat::Compat<TcpStream>>> {
    let host = env_get("MSSQL_HOST", env_file).ok_or_else(|| anyhow!("MSSQL_HOST requerido"))?;
    let port: u16 = env_get("MSSQL_PORT", env_file)
        .unwrap_or_else(|| "1433".into())
        .parse()
        .context("MSSQL_PORT invalido")?;

    let user = env_get("MSSQL_USER", env_file).ok_or_else(|| anyhow!("MSSQL_USER requerido"))?;
    let pass = env_get("MSSQL_PASSWORD", env_file)
        .or_else(|| env_get("MSSQL_PASS", env_file))
        .ok_or_else(|| anyhow!("MSSQL_PASSWORD requerido"))?;

    let db = env_get("MSSQL_DATABASE", env_file)
        .or_else(|| env_get("MSSQL_DB", env_file))
        .unwrap_or_else(|| "aval_cob".into());

    let encrypt = env_get("MSSQL_ENCRYPT", env_file)
        .map(|v| parse_bool(&v))
        .unwrap_or(false);

    let trust_cert = env_get("MSSQL_TRUST_CERT", env_file)
        .map(|v| {
            let v = v.trim().to_lowercase();
            if v == "off" || v == "false" || v == "0" {
                false
            } else {
                true
            }
        })
        .unwrap_or(true);

    let mut cfg = Config::new();
    cfg.host(host);
    cfg.port(port);
    cfg.authentication(tiberius::AuthMethod::sql_server(user, pass));
    cfg.database(db);

    if trust_cert {
        cfg.trust_cert();
    }

    // Evita handshake cuando el servidor no soporta TLS en ese puerto
    cfg.encryption(if encrypt {
        tiberius::EncryptionLevel::Required
    } else {
        tiberius::EncryptionLevel::NotSupported
    });

    let tcp = TcpStream::connect(cfg.get_addr()).await.context("TCP connect")?;
    tcp.set_nodelay(true).ok();
    let client = Client::connect(cfg, tcp.compat_write()).await.context("TDS connect")?;
    Ok(client)
}

async fn query_ints_first_col(client: &mut Client<tokio_util::compat::Compat<TcpStream>>, sql: &str) -> Result<Vec<i32>> {
    let mut out = Vec::new();
    let mut stream = client.simple_query(sql).await?;
    while let Some(item) = stream.try_next().await? {
        if let tiberius::QueryItem::Row(row) = item {
            let v: Option<i32> = row.try_get(0)?;
            if let Some(x) = v {
                out.push(x);
            }
        }
    }
    Ok(out)
}

#[derive(Debug, Deserialize, Clone)]
struct TailYaml {
    pub tail_sql: String,
}

fn load_tail_sql(path: &Path) -> Result<String> {
    let s = fs::read_to_string(path).with_context(|| format!("leer tail_sql_file {}", path.display()))?;
    let y: TailYaml = serde_yaml::from_str(&s).context("parse tail yaml")?;
    Ok(y.tail_sql)
}

#[derive(Debug, Deserialize, Clone)]
struct AgentsFile {
    pub version: u32,
    pub anexos: HashMap<String, String>,
    pub nombres: HashMap<String, String>,
}

fn load_agents(cfg: &ClientConfigFile) -> Result<Option<AgentsFile>> {
    let Some(a) = cfg.agents.as_ref() else { return Ok(None); };
    let p = PathBuf::from(&a.file);
    let s = fs::read_to_string(&p).with_context(|| format!("leer agents.yml {}", p.display()))?;
    let af: AgentsFile = serde_yaml::from_str(&s).context("parse agents.yml")?;
    Ok(Some(af))
}

#[derive(Debug, Deserialize, Clone)]
struct RptaOpeCodOutFile {
    pub version: u32,
    pub default: String,
    pub groups: HashMap<String, Vec<i32>>,
}

#[derive(Clone)]
struct RptaOpeCodOutIndex {
    pub default: String,
    pub by_id: HashMap<i32, String>,
}

impl RptaOpeCodOutIndex {
    fn resolve(&self, nid: Option<i32>) -> String {
        if let Some(id) = nid {
            if let Some(g) = self.by_id.get(&id) {
                return g.clone();
            }
        }
        self.default.clone()
    }
}

fn load_rpta_opecodout(cfg: &ClientConfigFile) -> Result<Option<RptaOpeCodOutIndex>> {
    let Some(rcfg) = cfg.rpta_opecodout.as_ref() else { return Ok(None); };
    let p = PathBuf::from(&rcfg.file);
    let s = fs::read_to_string(&p).with_context(|| format!("leer rpta_opecodout.yml {}", p.display()))?;
    let rf: RptaOpeCodOutFile = serde_yaml::from_str(&s).context("parse rpta_opecodout.yml")?;
    let mut by_id: HashMap<i32, String> = HashMap::new();
    for (group, ids) in rf.groups.iter() {
        for id in ids.iter() {
            by_id.entry(*id).or_insert_with(|| group.clone());
        }
    }
    Ok(Some(RptaOpeCodOutIndex { default: rf.default, by_id }))
}

fn build_batch_sql(
    date: &str,
    client_id: i32,
    window_sec: i64,
    carteras: &[i32],
    tail_sql: &str,
    inputs: &[(i32, u8, i32, String, String, Option<String>)],
) -> String {
    let mut sql = String::new();
    sql.push_str("SET NOCOUNT ON;\n");
    sql.push_str("CREATE TABLE #temporal (\n");
    sql.push_str("  k INT NOT NULL,\n");
    sql.push_str("  tipo TINYINT NOT NULL,\n");
    sql.push_str("  id_agente INT NOT NULL,\n");
    sql.push_str("  telefono VARCHAR(32) NOT NULL,\n");
    sql.push_str("  fecha_gestion DATETIME2(0) NOT NULL,\n");
    sql.push_str("  cid_llamada VARCHAR(128) NULL\n");
    sql.push_str(");\n");

    const INSERT_CHUNK: usize = 900;
    for chunk in inputs.chunks(INSERT_CHUNK) {
        sql.push_str("INSERT INTO #temporal (k,tipo,id_agente,telefono,fecha_gestion,cid_llamada) VALUES\n");
        for (i, (k, tipo, id_agente, telefono, fecha_gestion, cid)) in chunk.iter().enumerate() {
            let cid_sql = match cid {
                Some(s) => format!("'{}'", s.replace('\'', "''")),
                None => "NULL".to_string(),
            };
            let line = format!(
                "({}, {}, {}, '{}', '{}', {})",
                k,
                tipo,
                id_agente,
                telefono.replace('\'', "''"),
                fecha_gestion.replace('\'', "''"),
                cid_sql
            );
            sql.push_str(&line);
            sql.push_str(if i + 1 == chunk.len() { ";\n" } else { ",\n" });
        }
    }

    let carteras_list = if carteras.is_empty() {
        "NULL".to_string()
    } else {
        carteras.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",")
    };

    let mut tail = tail_sql.to_string();
    tail = tail.replace("{{WINDOW_SEC}}", &window_sec.to_string());
    tail = tail.replace("{{CLIENT_ID}}", &client_id.to_string());
    tail = tail.replace("{{DATE}}", date);
    tail = tail.replace("{{CARTERAS_LIST}}", &carteras_list);

    sql.push_str(&tail);
    sql
}

#[derive(Clone)]
struct Input {
    k: i32,
    record_id: String,
    tipo: u8,
    telefono: String,
    id_agente: i32,
    cid_llamada: Option<String>,
    anexo: Option<String>,
    parse_ok: bool,
    dt: NaiveDateTime,
}

fn map_client_data(cfg: &ClientConfigFile, row: &serde_json::Map<String, serde_json::Value>) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for f in cfg.json.fields.iter() {
        let v = row.get(&f.col).cloned().unwrap_or(serde_json::Value::Null);
        out.insert(f.key.clone(), v);
    }
    serde_json::Value::Object(out)
}

fn load_fetch_map(path: &Path) -> Result<HashMap<String, String>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let s = fs::read_to_string(path)?;
    let mf: FetchManifest = serde_json::from_str(&s).context("parse fetch.json")?;
    let mut m = HashMap::new();
    for it in mf.items.iter() {
        let record_id = util::record_id_from_filename(&it.filename);
        m.insert(record_id, it.filename.clone());
    }
    Ok(m)
}

fn load_probe_map(path: &Path) -> Result<HashMap<String, (bool, Option<f64>)>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let s = fs::read_to_string(path)?;
    let mf: ConvertManifest = serde_json::from_str(&s).context("parse convert.json")?;
    let mut m = HashMap::new();
    for it in mf.items.iter() {
        m.insert(it.record_id.clone(), (it.ffprobe_ok, it.duration_sec));
    }
    Ok(m)
}

fn load_client_cfg(shared_root: &Path, clients_dir: &str, client_code: &str) -> Result<ClientConfigFile> {
    let p = shared_root.join(clients_dir).join(format!("{client_code}.yml"));
    let s = fs::read_to_string(&p).with_context(|| format!("leer client cfg {}", p.display()))?;
    let cfg: ClientConfigFile = serde_yaml::from_str(&s).context("parse client cfg")?;
    Ok(cfg)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();

    let args = Args::parse();
    let shared_root = PathBuf::from(&args.shared_root);
    let cfg = load_client_cfg(&shared_root, &args.clients_dir, &args.client)?;
    let agents = load_agents(&cfg)?;
    let rpta_opecodout = load_rpta_opecodout(&cfg)?;


    let run_id = args.run_id.unwrap_or_else(|| format!("{}", Utc::now().format("%Y%m%dT%H%M%SZ")));
    let run_dir = paths::run_dir(&shared_root.join("runs"), &args.client, &args.date, &run_id);

    let wav_dir = paths::wav_dir(&run_dir);
    let matched_dir = paths::matched_dir(&run_dir);
    let manifests_dir = paths::manifests_dir(&run_dir);
    fs::create_dir_all(&matched_dir)?;
    fs::create_dir_all(&manifests_dir)?;

    // Map record_id -> source filename (raw input)
    let fetch_map = load_fetch_map(&manifests_dir.join("fetch.json"))?;
    // Map record_id -> probe info
    let probe_map = load_probe_map(&manifests_dir.join("convert.json"))?;

    // inputs desde wav/*.wav
    let pattern = wav_dir.join("*.wav").to_string_lossy().to_string();
    let mut inputs: Vec<Input> = vec![];
    let mut sql_inputs: Vec<(i32, u8, i32, String, String, Option<String>)> = vec![];
    let mut k = 0i32;

    for entry in glob(&pattern)? {
        let wav = entry?;
        let filename = wav.file_name().unwrap().to_string_lossy().to_string();

        let parsed = util::detect_tipo_and_parse(&filename);
        let (tipo, record_id, dt, id_raw, telefono, cid, anexo, parse_ok) = match parsed {
            Ok((tipo, record_id, dt, id_ag, tel, cid, anexo)) => (tipo, record_id, dt, id_ag, tel, cid, anexo, true),
            Err(_) => (1u8, util::record_id_from_filename(&filename), NaiveDateTime::from_timestamp_opt(0, 0).unwrap(), 0, "0".into(), None, None, false),
        };

        // resolver id_agente por anexo si aplica
        let mut id_agente = id_raw;
        if tipo == 1 {
            if let (Some(ax), Some(ag)) = (anexo.as_ref(), agents.as_ref()) {
                if let Some(id_str) = ag.anexos.get(ax) {
                    if let Ok(v) = id_str.parse::<i32>() {
                        id_agente = v;
                    }
                }
            }
        }

        inputs.push(Input {
            k,
            record_id: record_id.clone(),
            tipo,
            telefono: telefono.clone(),
            id_agente,
            cid_llamada: cid.clone(),
            anexo: anexo.clone(),
            parse_ok,
            dt,
        });

        let fecha_sql = util::format_naive_dt_sql(&dt);
        sql_inputs.push((k, tipo, id_agente, telefono, fecha_sql, cid));
        k += 1;
    }

    if inputs.is_empty() {
        tracing::warn!("no hay wavs en {}", wav_dir.display());
        return Ok(());
    }

    if args.dry_run {
        tracing::info!("--dry-run: no se consulta MSSQL ni se escriben jsons");
        return Ok(());
    }

    let env_file = read_kv_env_file(Path::new(&args.mssql_env_file));
    let mut client = mssql_connect(&env_file).await?;

    // carteras
    let mut car_sql = cfg.r#match.carteras_sql.clone();
    car_sql = car_sql.replace("{{CLIENT_ID}}", &cfg.client.id.to_string());
    car_sql = car_sql.replace("{{DATE}}", &args.date);
    let carteras = query_ints_first_col(&mut client, &car_sql).await?;

    let mut rows_map: HashMap<i32, serde_json::Map<String, serde_json::Value>> = HashMap::new();

    if !carteras.is_empty() {
        let tail_sql = load_tail_sql(Path::new(&cfg.r#match.batch_lookup.tail_sql_file))?;
        let sql = build_batch_sql(&args.date, cfg.client.id, cfg.r#match.window_sec_tipo1, &carteras, &tail_sql, &sql_inputs);

        let mut stream = client.simple_query(sql).await?;
        while let Some(item) = stream.try_next().await? {
            let row = match item {
                tiberius::QueryItem::Row(r) => r,
                _ => continue,
            };

            let k_val: Option<i32> = row.try_get(0)?;
            let k_i = k_val.unwrap_or(-1);

            let mut obj = serde_json::Map::new();
            for (idx, col) in row.columns().iter().enumerate().skip(1) {
                let name = col.name().to_string();

                if let Ok(v) = row.try_get::<&str, _>(idx) {
                    if let Some(s) = v {
                        obj.insert(name, serde_json::Value::String(s.to_string()));
                        continue;
                    }
                }
                if let Ok(v) = row.try_get::<i32, _>(idx) {
                    if let Some(i) = v {
                        obj.insert(name, serde_json::Value::from(i));
                        continue;
                    }
                }
                if let Ok(v) = row.try_get::<f64, _>(idx) {
                    if let Some(f) = v {
                        obj.insert(name, serde_json::Value::from(f));
                        continue;
                    }
                }
                obj.insert(name, serde_json::Value::Null);
            }

            rows_map.insert(k_i, obj);
        }
    }

    // escribir jsons
    let mut items: Vec<MatchItem> = vec![];
    for inp in inputs.iter() {
        let record_id = &inp.record_id;
        let json_rel = format!("matched/{record_id}.json");
        let wav_rel = format!("wav/{record_id}.wav");

        let row = rows_map.get(&inp.k).cloned().unwrap_or_default();
        let lookup_ok = !row.is_empty();
        let data = map_client_data(&cfg, &row);

        // source filename/raw path (si existe en fetch.json)
        let source_filename = fetch_map.get(record_id).cloned();
        let raw_path = source_filename.as_ref().map(|f| format!("raw/{f}"));

        // fechas
        let fecha_gestion_parse = inp.dt.format("%Y-%m-%d %H:%M:%S").to_string();
        let fecha_gestion = inp.dt.date().format("%Y-%m-%d").to_string();
        let hora = inp.dt.time().format("%H:%M:%S").to_string();

        // agente
        let (nombre_agente, mapping_ok) = if let Some(ag) = agents.as_ref() {
            let key = inp.id_agente.to_string();
            let name = ag.nombres.get(&key).cloned();
            let ok = name.is_some();
            (name, ok)
        } else {
            (None, false)
        };

        // probe
        let (ffprobe_ok, duration_sec) = probe_map.get(record_id).cloned().unwrap_or((false, None));
        // rpta (descrip_rpta por nId_OpeCodOut)
        let nid_opecodout = row.get("NID_OPECODOUT").and_then(|v| v.as_i64()).map(|v| v as i32);
        let descrip_rpta = rpta_opecodout
            .as_ref()
            .map(|m| m.resolve(nid_opecodout))
            .unwrap_or_else(|| "OTRO".to_string());


        let core = serde_json::json!({
          "schema_version": 1,
          "client": { "id": cfg.client.id, "code": cfg.client.code.clone() },
          "run": { "run_id": run_id.clone(), "date": args.date.clone(), "generated_at": Utc::now().to_rfc3339() },
          "audio": {
            "record_id": record_id,
            "source_filename": source_filename,
            "raw_path": raw_path,
            "wav_path": wav_rel
          },
          "call": {
            "tipo": inp.tipo,
            "telefono": inp.telefono,
            "id_agente": inp.id_agente,
            "cid_llamada": inp.cid_llamada,
            "anexo": inp.anexo,
            "parse_ok": inp.parse_ok,
            "fecha_gestion_parse": fecha_gestion_parse,
            "fecha_gestion": fecha_gestion,
            "hora": hora
          },
          "agent": {
            "nombre_agente": nombre_agente,
            "mapping_ok": mapping_ok,
            "mapping_source": "yaml"
          },
          "probe": {
            "ffprobe_ok": ffprobe_ok,
            "duration_sec": duration_sec
          },
          "lookup": {
            "ok": lookup_ok,
            "client_id": cfg.client.id,
            "date": args.date.clone(),
            "window_sec": cfg.r#match.window_sec_tipo1,
            "carteras_count": carteras.len()
          },
          "rpta": {
            "descrip_rpta": descrip_rpta
          },

          "data": data
        });

        let out_path = run_dir.join(&json_rel);
        fs::create_dir_all(out_path.parent().unwrap())?;
        fs::write(&out_path, serde_json::to_string_pretty(&core)?)?;

        items.push(MatchItem {
            record_id: record_id.clone(),
            wav_path: wav_rel.clone(),
            json_path: json_rel,
            lookup_ok,
        });
    }

    let manifest = MatchManifest {
        schema_version: 1,
        client: args.client.clone(),
        date: args.date.clone(),
        run_id: run_id.clone(),
        generated_at: Utc::now(),
        items,
    };
    fs::write(manifests_dir.join("match.json"), serde_json::to_string_pretty(&manifest)?)?;
    println!("run_dir={}", run_dir.display());
    Ok(())
}
