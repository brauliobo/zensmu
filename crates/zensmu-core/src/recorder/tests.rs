use super::csv::parse_csv_records;
use super::*;
use crate::types::Codename;
use std::fs;
use std::path::Path;

fn sample_snapshot(ppt: f32, tctl: f32) -> TelemetrySnapshot {
    TelemetrySnapshot {
        codename: Codename::GraniteRidge,
        table_version: 0x0062_0105,
        table_size: 0x724,
        ppt_limit_w: Some(142.0),
        ppt_value_w: Some(ppt),
        tdc_limit_a: Some(110.0),
        tdc_value_a: Some(65.5),
        edc_limit_a: Some(170.0),
        edc_value_a: Some(92.25),
        tctl_limit_c: Some(95.0),
        tctl_value_c: Some(tctl),
        stapm_limit_w: None,
        stapm_value_w: None,
        slow_limit_w: None,
        slow_value_w: None,
        apu_slow_limit_w: None,
        apu_slow_value_w: None,
        tdc_soc_limit_a: None,
        tdc_soc_value_a: None,
        edc_soc_limit_a: None,
        edc_soc_value_a: None,
        apu_skin_temp_limit_c: None,
        apu_skin_temp_value_c: None,
        dgpu_skin_temp_limit_c: None,
        dgpu_skin_temp_value_c: None,
        fclk_mhz: Some(2000.0),
        uclk_mhz: Some(3000.0),
        mclk_mhz: Some(3000.0),
        vddcr_soc_v: Some(1.245),
        vddcr_misc_v: Some(1.1),
        cldo_vddp_v: Some(0.95),
        cldo_vddg_iod_v: Some(0.95),
        cldo_vddg_ccd_v: Some(0.95),
    }
}

#[test]
fn csv_recording_writes_header_once_and_appends_without_duplicate_header() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("telemetry.csv");
    let expected_cols = CSV_HEADER.split(',').count();

    {
        let mut rec = TelemetryRecorder::open(RecordConfig {
            path: path.clone(),
            format: RecordFormat::Csv,
            append: true,
            max_file_bytes: None,
            max_rotated_files: 0,
            flush_every: 1,
            tag: Some("prime95, \"small-fft\"".to_string()),
        })
        .unwrap();
        rec.write_sample_at(&sample_snapshot(88.5, 72.25), 1_700_000_000_125)
            .unwrap();
        assert_eq!(rec.stats().samples_written, 1);
        assert_eq!(rec.format(), RecordFormat::Csv);
        assert_eq!(rec.path(), path.as_path());
    }

    // Re-open in append mode: must NOT write a duplicate header.
    {
        let mut rec = TelemetryRecorder::open(RecordConfig {
            path: path.clone(),
            format: RecordFormat::Csv,
            append: true,
            max_file_bytes: None,
            max_rotated_files: 0,
            flush_every: 1,
            tag: None,
        })
        .unwrap();
        rec.write_sample_at(&sample_snapshot(91.0, 74.5), 1_700_000_001_500)
            .unwrap();
    }

    let raw = fs::read_to_string(&path).unwrap();
    let records = parse_csv_records(&raw).unwrap();
    assert_eq!(records.len(), 3, "expected 1 header + 2 data rows");
    assert_eq!(records[0].join(","), CSV_HEADER);
    assert_eq!(records[1].len(), expected_cols);
    assert_eq!(records[2].len(), expected_cols);

    // Verify RFC 4180 tag quoting in raw line and parsed field value.
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(lines[1].contains("\"prime95, \"\"small-fft\"\"\""));
    assert_eq!(records[1][2], "granite-ridge");
    assert_eq!(records[1][3], "prime95, \"small-fft\"");
    assert_eq!(records[1][4], "88.50");
    assert_eq!(records[1][5], "142.00");
    assert_eq!(records[1][10], "72.25");
    assert_eq!(records[1][11], "95.00");
    // Optional APU fields must be empty strings when None.
    assert_eq!(records[1][12], "");
    assert_eq!(records[1][13], "");
    // Voltage fields have 4 decimal places.
    assert_eq!(records[1][29], "1.2450");

    // Second row has empty tag.
    assert_eq!(records[2][3], "");
    assert_eq!(records[2][4], "91.00");
}

#[test]
fn jsonl_recording_parses_back_cleanly_per_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("telemetry.jsonl");
    assert_eq!(RecordFormat::from_path(&path), RecordFormat::Jsonl);
    assert_eq!(
        RecordFormat::from_path(Path::new("run.ndjson")),
        RecordFormat::Jsonl
    );

    let mut rec = TelemetryRecorder::open(RecordConfig {
        path: path.clone(),
        format: RecordFormat::Jsonl,
        append: false,
        max_file_bytes: None,
        max_rotated_files: 0,
        flush_every: 2,
        tag: Some("co-20".to_string()),
    })
    .unwrap();

    rec.write_sample_at(&sample_snapshot(80.0, 65.0), 1_700_000_000_000)
        .unwrap();
    rec.write_sample_at(&sample_snapshot(105.25, 78.75), 1_700_000_002_250)
        .unwrap();
    rec.flush().unwrap();

    let content = fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 2);

    let s0: RecordedSample = serde_json::from_str(lines[0]).unwrap();
    let s1: RecordedSample = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(s0.timestamp_unix_ms, 1_700_000_000_000);
    assert_eq!(s0.tag.as_deref(), Some("co-20"));
    assert_eq!(s0.telemetry.codename, Codename::GraniteRidge);
    assert_eq!(s0.telemetry.ppt_value_w, Some(80.0));
    assert_eq!(s1.timestamp_unix_ms, 1_700_000_002_250);
    assert_eq!(s1.telemetry.ppt_value_w, Some(105.25));
    assert_eq!(s1.telemetry.tctl_value_c, Some(78.75));
}

#[test]
fn size_based_rotation_shifts_numbered_backups_and_rewrites_csv_header() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("metrics.csv");

    // Header is ~374 bytes, each row is ~145 bytes. Set max_file_bytes so
    // 1 sample fits per file and each subsequent sample triggers rotation.
    let max_bytes = (CSV_HEADER.len() + 1 + 220) as u64;

    let mut rec = TelemetryRecorder::open(RecordConfig {
        path: path.clone(),
        format: RecordFormat::Csv,
        append: false,
        max_file_bytes: Some(max_bytes),
        max_rotated_files: 2,
        flush_every: 1,
        tag: Some("stress".to_string()),
    })
    .unwrap();

    rec.write_sample_at(&sample_snapshot(50.0, 55.0), 1_700_000_001_000)
        .unwrap();
    rec.write_sample_at(&sample_snapshot(60.0, 60.0), 1_700_000_002_000)
        .unwrap();
    rec.write_sample_at(&sample_snapshot(70.0, 65.0), 1_700_000_003_000)
        .unwrap();
    rec.write_sample_at(&sample_snapshot(80.0, 70.0), 1_700_000_004_000)
        .unwrap();

    let stats = rec.stats();
    assert_eq!(stats.samples_written, 4);
    assert_eq!(stats.rotations, 3);

    let active = fs::read_to_string(&path).unwrap();
    let b1 = fs::read_to_string(dir.path().join("metrics.1.csv")).unwrap();
    let b2 = fs::read_to_string(dir.path().join("metrics.2.csv")).unwrap();
    assert!(!dir.path().join("metrics.3.csv").exists());

    for (label, content, expected_ppt) in [
        ("active", &active, "80.00"),
        ("backup1", &b1, "70.00"),
        ("backup2", &b2, "60.00"),
    ] {
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2, "{label} should have header + 1 row");
        assert_eq!(lines[0], CSV_HEADER, "{label} must start with CSV_HEADER");
        assert!(
            lines[1].contains(expected_ppt),
            "{label} expected ppt {expected_ppt}, got: {}",
            lines[1]
        );
    }
}

#[test]
fn iso8601_formatting_handles_epoch_and_leap_year() {
    assert_eq!(format_iso8601_utc_ms(0), "1970-01-01T00:00:00.000Z");
    assert_eq!(format_iso8601_utc_ms(123), "1970-01-01T00:00:00.123Z");
    // 2000-02-29T12:34:56.789Z (leap century divisible by 400)
    // Days from 1970-01-01 to 2000-02-29 = 30*365 + 7 leap years (72..96) + 31 (Jan) + 28 (Feb) = 11016
    let y2k_leap_ms = (11_016u64 * 86_400 + 12 * 3600 + 34 * 60 + 56) * 1_000 + 789;
    assert_eq!(
        format_iso8601_utc_ms(y2k_leap_ms),
        "2000-02-29T12:34:56.789Z"
    );
    // 2024-02-29T23:59:59.999Z
    // Days from 1970-01-01 to 2024-02-29 = 54*365 + 13 leap years + 31 + 28 = 19782
    let leap_2024_ms = (19_782u64 * 86_400 + 86_399) * 1_000 + 999;
    assert_eq!(
        format_iso8601_utc_ms(leap_2024_ms),
        "2024-02-29T23:59:59.999Z"
    );
    // Next millisecond rolls over to 2024-03-01T00:00:00.000Z
    assert_eq!(
        format_iso8601_utc_ms(leap_2024_ms + 1),
        "2024-03-01T00:00:00.000Z"
    );
}

#[test]
fn pts_recording_writes_valid_composite_xml_with_escaping_and_stats() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bench-run").join("composite.xml");
    assert_eq!(RecordFormat::from_path(&path), RecordFormat::Pts);
    assert_eq!(RecordFormat::Pts.extension(), "xml");
    assert_eq!(RecordFormat::Pts.as_str(), "pts");

    let pts_path = pts_result_path("my run/v1.0 <test>");
    assert!(pts_path
        .display()
        .to_string()
        .ends_with("test-results/my-run-v1-0--test-/composite.xml"));

    {
        let mut rec = TelemetryRecorder::open(RecordConfig {
            path: path.clone(),
            format: RecordFormat::Pts,
            append: false,
            max_file_bytes: None,
            max_rotated_files: 0,
            flush_every: 1,
            tag: Some("co-20 & <\"stress\"> 'run'".to_string()),
        })
        .unwrap();

        rec.write_sample_at(&sample_snapshot(80.0, 65.0), 1_700_000_000_000)
            .unwrap();
        rec.write_sample_at(&sample_snapshot(100.0, 75.0), 1_700_000_001_000)
            .unwrap();
        rec.flush().unwrap();
        assert_eq!(rec.stats().samples_written, 2);
        assert!(rec.stats().bytes_written > 0);
    }

    let xml = fs::read_to_string(&path).unwrap();
    assert!(xml.starts_with("<?xml version=\"1.0\"?>"));
    assert!(xml.contains("<PhoronixTestSuite>"));
    assert!(xml.contains("</PhoronixTestSuite>"));
    assert!(xml.contains("<Result>"));
    assert!(xml.contains("<Identifier>zensmu/ppt</Identifier>"));
    assert!(xml.contains("<Title>Package Power (PPT)</Title>"));
    assert!(xml.contains("<Scale>Watts</Scale>"));
    assert!(xml.contains("<Proportion>LIB</Proportion>"));
    assert!(xml.contains("<DisplayFormat>LINE_GRAPH</DisplayFormat>"));
    // Mean of 80.0 and 100.0 is 90.00
    assert!(xml.contains("<Value>90.00</Value>"));
    assert!(xml.contains("<RawString>80.00:100.00</RawString>"));
    assert!(xml.contains("<JSON>{\"min\":80.00,\"max\":100.00,\"avg\":90.00,\"samples\":2}</JSON>"));
    // Clock metric should be Higher Is Better (HIB)
    assert!(xml.contains("<Identifier>zensmu/fclk</Identifier>"));
    assert!(xml.contains("<Scale>MHz</Scale>"));
    assert!(xml.contains("<Proportion>HIB</Proportion>"));
    // XML escaping of &, <, >, ", '
    assert!(xml.contains("co-20 &amp; &lt;&quot;stress&quot;&gt; &apos;run&apos;"));
    // Unused APU-only metrics should not be emitted
    assert!(!xml.contains("zensmu/stapm"));
}

#[test]
fn convert_jsonl_or_csv_to_pts_handles_both_formats() {
    let dir = tempfile::tempdir().unwrap();
    let csv_path = dir.path().join("input.csv");
    let jsonl_path = dir.path().join("input.jsonl");
    let csv_pts_out = dir.path().join("from_csv.xml");
    let jsonl_pts_out = dir.path().join("from_jsonl.xml");

    {
        let mut csv_rec = TelemetryRecorder::open(RecordConfig {
            path: csv_path.clone(),
            format: RecordFormat::Csv,
            append: false,
            max_file_bytes: None,
            max_rotated_files: 0,
            flush_every: 1,
            tag: Some("cinebench, r23".to_string()),
        })
        .unwrap();
        csv_rec
            .write_sample_at(&sample_snapshot(90.0, 70.0), 1_700_000_000_000)
            .unwrap();
        csv_rec
            .write_sample_at(&sample_snapshot(110.0, 80.0), 1_700_000_001_000)
            .unwrap();
    }

    {
        let mut jsonl_rec = TelemetryRecorder::open(RecordConfig {
            path: jsonl_path.clone(),
            format: RecordFormat::Jsonl,
            append: false,
            max_file_bytes: None,
            max_rotated_files: 0,
            flush_every: 1,
            tag: Some("cinebench, r23".to_string()),
        })
        .unwrap();
        jsonl_rec
            .write_sample_at(&sample_snapshot(90.0, 70.0), 1_700_000_000_000)
            .unwrap();
        jsonl_rec
            .write_sample_at(&sample_snapshot(110.0, 80.0), 1_700_000_001_000)
            .unwrap();
    }

    let csv_count = convert_jsonl_or_csv_to_pts(
        &csv_path,
        &csv_pts_out,
        Some("R23 & Thermal <Test>"),
        Some("9700x-co-20"),
    )
    .unwrap();
    let jsonl_count = convert_jsonl_or_csv_to_pts(
        &jsonl_path,
        &jsonl_pts_out,
        Some("R23 & Thermal <Test>"),
        Some("9700x-co-20"),
    )
    .unwrap();

    assert_eq!(csv_count, 2);
    assert_eq!(jsonl_count, 2);

    let csv_xml = fs::read_to_string(&csv_pts_out).unwrap();
    let jsonl_xml = fs::read_to_string(&jsonl_pts_out).unwrap();

    for xml in [&csv_xml, &jsonl_xml] {
        assert!(xml.contains("<PhoronixTestSuite>"));
        assert!(xml.contains("<Title>R23 &amp; Thermal &lt;Test&gt;</Title>"));
        assert!(xml.contains("<Identifier>9700x-co-20</Identifier>"));
        assert!(xml.contains("<Arguments>cinebench, r23</Arguments>"));
        assert!(xml.contains("<Value>100.00</Value>"));
        assert!(xml.contains("<RawString>90.00:110.00</RawString>"));
        assert!(xml.contains("<Value>75.00</Value>"));
        assert!(xml.contains("<RawString>70.00:80.00</RawString>"));
    }
}
