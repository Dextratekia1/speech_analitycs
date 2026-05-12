use anyhow::{anyhow, Result};
use chrono::{Datelike, NaiveDate, NaiveDateTime};

pub fn parse_date_ymd(s: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|e| anyhow!("invalid --date '{s}': {e}"))
}

pub fn expand_source_template(t: &str, date: &NaiveDate) -> String {
    let yyyy = format!("{:04}", date.year());
    let mm = format!("{:02}", date.month());
    let dd = format!("{:02}", date.day());
    let yyyymm = format!("{yyyy}{mm}");
    t.replace("{YYYY}", &yyyy)
        .replace("{MM}", &mm)
        .replace("{DD}", &dd)
        .replace("{YYYYMM}", &yyyymm)
}

pub fn format_naive_dt_sql(dt: &NaiveDateTime) -> String {
    dt.format("%Y-%m-%d %H:%M:%S").to_string()
}

pub fn record_id_from_filename(filename: &str) -> String {
    filename.rsplit_once('.').map(|(a, _)| a.to_string()).unwrap_or_else(|| filename.to_string())
}
fn normalize_tipo1_phone(raw: &str) -> String {
    let s = raw.trim();
    if s.len() == 10 && s.starts_with('8') {
        s[1..].to_string()
    } else {
        s.to_string()
    }
}


pub fn detect_tipo_and_parse(filename: &str) -> Result<(u8, String, NaiveDateTime, i32, String, Option<String>, Option<String>)> {
    // Returns: (tipo, record_id, fecha_gestion, id_agente, telefono, cid_llamada, anexo)
    let record_id = record_id_from_filename(filename);
    // Tipo 1 (OUT): OUT-YYYYMMDD-HHMMSS-ANEXO-ANI(.ext)
    if record_id.starts_with("OUT-") {
        let parts: Vec<&str> = record_id.split('-').collect();
        if parts.len() == 5 {
            let date_raw = parts[1];
            let time_raw = parts[2];
            let anexo = parts[3].to_string();
            let telefono = normalize_tipo1_phone(parts[4]);
            let dt = NaiveDateTime::parse_from_str(&format!("{date_raw} {time_raw}"), "%Y%m%d %H%M%S")
                .map_err(|e| anyhow!("fecha/hora invalida en '{filename}': {e}"))?;
            // id_agente se resuelve por mapping de anexo en el matcher; aquí se deja 0
            return Ok((1u8, record_id, dt, 0, telefono, None, Some(anexo)));
        }
    }

    let parts: Vec<&str> = record_id.split('_').collect();
    if parts.len() < 6 {
        return Err(anyhow!("filename '{filename}' no cumple formato esperado (>=6 partes separadas por _)"));
    }

    let is_tipo1 = parts[0].chars().all(|c| c.is_ascii_digit());
    let (tipo, idx_ani, idx_date, idx_time, idx_agente, cid) = if is_tipo1 {
        (1u8, parts.len()-4, parts.len()-3, parts.len()-2, parts.len()-1, None)
    } else {
        (2u8, 2usize, 3usize, 4usize, 5usize, Some(parts[0].to_string()))
    };

    let telefono = if tipo == 1 {
        normalize_tipo1_phone(parts[idx_ani])
    } else {
        parts[idx_ani].to_string()
    };
    let date_raw = parts[idx_date];
    let time_raw = parts[idx_time];
    let id_agente: i32 = parts[idx_agente].parse().map_err(|e| anyhow!("id_agente invalido en '{filename}': {e}"))?;
    let dt = NaiveDateTime::parse_from_str(&format!("{date_raw} {time_raw}"), "%Y%m%d %H%M%S")
        .map_err(|e| anyhow!("fecha/hora invalida en '{filename}': {e}"))?;
    Ok((tipo, record_id, dt, id_agente, telefono, cid, None))
}
