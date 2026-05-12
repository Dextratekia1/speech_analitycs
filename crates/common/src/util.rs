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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn dt(ymd_hms: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(ymd_hms, "%Y%m%d %H%M%S").unwrap()
    }

    // --- record_id_from_filename ---

    #[test]
    fn test_record_id_strips_gsm() {
        assert_eq!(record_id_from_filename("audio.gsm"), "audio");
    }

    #[test]
    fn test_record_id_strips_wav() {
        assert_eq!(record_id_from_filename("audio.wav"), "audio");
    }

    #[test]
    fn test_record_id_multiple_dots() {
        // rsplit_once splits at the LAST dot, so only the final extension is stripped
        assert_eq!(
            record_id_from_filename("file.with.multiple.dots.gsm"),
            "file.with.multiple.dots"
        );
    }

    #[test]
    fn test_record_id_no_extension() {
        assert_eq!(record_id_from_filename("noextension"), "noextension");
    }

    // --- expand_source_template ---
    // NOTE: The function replaces {YYYY}, {MM}, {DD}, {YYYYMM} (uppercase).
    // {client}, {date}, and lowercase variants are NOT handled by this function.
    // In the fetcher, {client} is substituted via a direct .replace() call on
    // base_url_template (audio-fetcher-rs/src/main.rs:80), not through this fn.

    #[test]
    fn test_expand_yyyy() {
        let d = NaiveDate::from_ymd_opt(2026, 1, 8).unwrap();
        assert_eq!(expand_source_template("{YYYY}", &d), "2026");
    }

    #[test]
    fn test_expand_mm_zero_padded() {
        let d = NaiveDate::from_ymd_opt(2026, 1, 8).unwrap();
        assert_eq!(expand_source_template("{MM}", &d), "01");
    }

    #[test]
    fn test_expand_dd_zero_padded() {
        let d = NaiveDate::from_ymd_opt(2026, 1, 8).unwrap();
        assert_eq!(expand_source_template("{DD}", &d), "08");
    }

    #[test]
    fn test_expand_yyyymm() {
        let d = NaiveDate::from_ymd_opt(2026, 1, 8).unwrap();
        assert_eq!(expand_source_template("{YYYYMM}", &d), "202601");
    }

    #[test]
    fn test_expand_all_placeholders_in_path() {
        let d = NaiveDate::from_ymd_opt(2026, 1, 8).unwrap();
        let tmpl = "OCM-MANUAL-{YYYY}/{MM}/{DD}";
        assert_eq!(expand_source_template(tmpl, &d), "OCM-MANUAL-2026/01/08");
    }

    #[test]
    fn test_expand_yyyymm_in_path() {
        let d = NaiveDate::from_ymd_opt(2026, 1, 8).unwrap();
        let tmpl = "entrantes{YYYY}/{YYYYMM}/{DD}";
        assert_eq!(expand_source_template(tmpl, &d), "entrantes2026/202601/08");
    }

    #[test]
    fn test_expand_lowercase_not_replaced() {
        // Lowercase variants are not placeholders — passed through unchanged.
        let d = NaiveDate::from_ymd_opt(2026, 1, 8).unwrap();
        assert_eq!(expand_source_template("{yyyy}", &d), "{yyyy}");
        assert_eq!(expand_source_template("{mm}", &d), "{mm}");
        assert_eq!(expand_source_template("{dd}", &d), "{dd}");
    }

    // --- parse_date_ymd ---

    #[test]
    fn test_parse_date_valid() {
        let d = parse_date_ymd("2026-01-08").unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 1, 8).unwrap());
    }

    #[test]
    fn test_parse_date_invalid_month() {
        assert!(parse_date_ymd("2026-13-01").is_err());
    }

    #[test]
    fn test_parse_date_malformed_string() {
        assert!(parse_date_ymd("not-a-date").is_err());
    }

    // --- format_naive_dt_sql ---

    #[test]
    fn test_format_naive_dt_sql() {
        let d = dt("20260108 143022");
        assert_eq!(format_naive_dt_sql(&d), "2026-01-08 14:30:22");
    }

    // --- detect_tipo_and_parse ---

    // Path 1: OUT-YYYYMMDD-HHMMSS-ANEXO-ANI(.ext) — tipo 1, id_agente hardcoded 0
    #[test]
    fn test_tipo1_out_format_basic() {
        let (tipo, record_id, fecha, id_agente, telefono, cid, anexo) =
            detect_tipo_and_parse("OUT-20260108-143022-201-987654321.gsm").unwrap();
        assert_eq!(tipo, 1);
        assert_eq!(record_id, "OUT-20260108-143022-201-987654321");
        assert_eq!(fecha, dt("20260108 143022"));
        assert_eq!(id_agente, 0); // hardcoded in OUT path; resolved via anexo mapping in matcher
        assert_eq!(telefono, "987654321"); // 9 chars — not normalized
        assert_eq!(cid, None);
        assert_eq!(anexo, Some("201".to_string()));
    }

    #[test]
    fn test_tipo1_out_phone_normalization_strips_leading_8() {
        // 10-char phone starting with '8' → strip the leading '8'
        let (_, _, _, _, telefono, _, _) =
            detect_tipo_and_parse("OUT-20260108-143022-202-8123456789.gsm").unwrap();
        assert_eq!(telefono, "123456789");
    }

    #[test]
    fn test_tipo1_out_phone_no_strip_when_not_8() {
        // 10-char phone that does NOT start with '8' → returned unchanged
        let (_, _, _, _, telefono, _, _) =
            detect_tipo_and_parse("OUT-20260108-143022-203-1234567890.gsm").unwrap();
        assert_eq!(telefono, "1234567890");
    }

    // Path 2: underscore format with numeric first part → tipo 1
    // Layout (6 parts): NUM_extra_ANI_YYYYMMDD_HHMMSS_AGENTE
    // Indices are relative to the END: ani=len-4, date=len-3, time=len-2, agente=len-1
    #[test]
    fn test_tipo1_underscore_format_basic() {
        // 6 parts: ["12345","somedata","9876543210","20260108","143022","42"]
        let (tipo, record_id, fecha, id_agente, telefono, cid, anexo) =
            detect_tipo_and_parse("12345_somedata_9876543210_20260108_143022_42.gsm").unwrap();
        assert_eq!(tipo, 1);
        assert_eq!(record_id, "12345_somedata_9876543210_20260108_143022_42");
        assert_eq!(fecha, dt("20260108 143022"));
        assert_eq!(id_agente, 42);
        assert_eq!(telefono, "9876543210"); // 10 chars starting with '9' — not stripped
        assert_eq!(cid, None);
        assert_eq!(anexo, None);
    }

    #[test]
    fn test_tipo1_underscore_phone_normalization() {
        // 10-char phone starting with '8' in numeric-prefix format → stripped
        let (_, _, _, _, telefono, _, _) =
            detect_tipo_and_parse("12345_somedata_8123456789_20260108_143022_42.gsm").unwrap();
        assert_eq!(telefono, "123456789");
    }

    // Path 3: underscore format with non-numeric first part → tipo 2 (inbound / CID)
    // Fixed indices: ani=2, date=3, time=4, agente=5; no phone normalization
    #[test]
    fn test_tipo2_cid_format_basic() {
        // 6 parts: ["CID12345","extra","987654321","20260108","143022","7"]
        let (tipo, record_id, fecha, id_agente, telefono, cid, anexo) =
            detect_tipo_and_parse("CID12345_extra_987654321_20260108_143022_7.gsm").unwrap();
        assert_eq!(tipo, 2);
        assert_eq!(record_id, "CID12345_extra_987654321_20260108_143022_7");
        assert_eq!(fecha, dt("20260108 143022"));
        assert_eq!(id_agente, 7);
        assert_eq!(telefono, "987654321"); // tipo 2 — no normalization even if 10 chars starting with '8'
        assert_eq!(cid, Some("CID12345".to_string()));
        assert_eq!(anexo, None);
    }

    #[test]
    fn test_tipo2_phone_not_normalized() {
        // Even a 10-char '8'-prefix phone is NOT normalized for tipo 2
        let (_, _, _, _, telefono, _, _) =
            detect_tipo_and_parse("CID99999_extra_8123456789_20260108_143022_3.gsm").unwrap();
        assert_eq!(telefono, "8123456789"); // unchanged — only tipo 1 normalizes
    }

    // Error cases
    #[test]
    fn test_err_too_few_underscore_parts() {
        // 2 parts < 6 → error (and does not start with "OUT-")
        assert!(detect_tipo_and_parse("abc_def.gsm").is_err());
    }

    #[test]
    fn test_err_malformed_datetime_out_format() {
        // Month 13 is invalid
        assert!(detect_tipo_and_parse("OUT-20261399-143022-201-987654321.gsm").is_err());
    }

    #[test]
    fn test_err_malformed_datetime_underscore_format() {
        // Month 13 is invalid
        assert!(detect_tipo_and_parse("12345_somedata_9876543210_20261399_143022_42.gsm").is_err());
    }

    #[test]
    fn test_err_id_agente_not_integer() {
        // id_agente field contains non-integer text
        assert!(
            detect_tipo_and_parse("CID12345_extra_987654321_20260108_143022_notanumber.gsm").is_err()
        );
    }
}
