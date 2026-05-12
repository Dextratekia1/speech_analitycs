use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct ClientConfigFile {
    pub version: u32,
    pub client: ClientIdentity,
    pub fetch: FetchConfig,
    pub r#match: MatchConfig,
    pub json: JsonConfig,
    pub agents: Option<AgentsConfig>,
    pub rpta_opecodout: Option<RptaOpeCodOutConfig>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AgentsConfig {
    pub file: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RptaOpeCodOutConfig {
    pub file: String,
}


#[derive(Debug, Deserialize, Clone)]
pub struct ClientIdentity {
    pub id: i32,
    pub code: String,
    pub name: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct FetchConfig {
    pub base_url_template: String,
    pub sources: Vec<String>,
    pub extensions: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct MatchConfig {
    pub window_sec_tipo1: i64,
    pub carteras_sql: String,
    pub batch_lookup: BatchLookupConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct BatchLookupConfig {
    pub dialect: String,
    pub tail_sql_file: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct JsonConfig {
    pub version: u32,
    pub fields: Vec<JsonField>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct JsonField {
    pub key: String,
    pub col: String,
    pub r#type: String,
}
