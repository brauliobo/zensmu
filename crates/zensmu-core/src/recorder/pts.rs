use super::csv::parse_csv_samples;
use super::paths::derive_pts_label_from_path;
use super::time::{format_pts_datetime_utc, now_unix_ms};
use super::{RecordFormat, RecordedSample};
use crate::error::{Error, Result};
use crate::ops::pm_table::TelemetrySnapshot;
use crate::report::SystemBoardInfo;
use crate::safety::{map_io_err, write_atomic};
use crate::types::Codename;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

/// XML-escape `&`, `<`, `>`, `"`, and `'`.
fn xml_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

struct PtsMetricSpec {
    slug: &'static str,
    title: &'static str,
    unit: &'static str,
    proportion: &'static str,
    precision: usize,
    extract: fn(&TelemetrySnapshot) -> Option<f32>,
}

const PTS_METRICS: &[PtsMetricSpec] = &[
    PtsMetricSpec {
        slug: "ppt",
        title: "Package Power (PPT)",
        unit: "Watts",
        proportion: "LIB",
        precision: 2,
        extract: |t| t.ppt_value_w,
    },
    PtsMetricSpec {
        slug: "tctl",
        title: "CPU Temperature (Tctl)",
        unit: "Celsius",
        proportion: "LIB",
        precision: 2,
        extract: |t| t.tctl_value_c,
    },
    PtsMetricSpec {
        slug: "tdc",
        title: "VDD TDC Current",
        unit: "Amps",
        proportion: "LIB",
        precision: 2,
        extract: |t| t.tdc_value_a,
    },
    PtsMetricSpec {
        slug: "edc",
        title: "VDD EDC Current",
        unit: "Amps",
        proportion: "LIB",
        precision: 2,
        extract: |t| t.edc_value_a,
    },
    PtsMetricSpec {
        slug: "stapm",
        title: "STAPM Power",
        unit: "Watts",
        proportion: "LIB",
        precision: 2,
        extract: |t| t.stapm_value_w,
    },
    PtsMetricSpec {
        slug: "slow-ppt",
        title: "Slow PPT",
        unit: "Watts",
        proportion: "LIB",
        precision: 2,
        extract: |t| t.slow_value_w,
    },
    PtsMetricSpec {
        slug: "apu-slow-ppt",
        title: "APU Slow PPT",
        unit: "Watts",
        proportion: "LIB",
        precision: 2,
        extract: |t| t.apu_slow_value_w,
    },
    PtsMetricSpec {
        slug: "tdc-soc",
        title: "SoC TDC",
        unit: "Amps",
        proportion: "LIB",
        precision: 2,
        extract: |t| t.tdc_soc_value_a,
    },
    PtsMetricSpec {
        slug: "edc-soc",
        title: "SoC EDC",
        unit: "Amps",
        proportion: "LIB",
        precision: 2,
        extract: |t| t.edc_soc_value_a,
    },
    PtsMetricSpec {
        slug: "apu-skin-temp",
        title: "APU Skin Temp",
        unit: "Celsius",
        proportion: "LIB",
        precision: 2,
        extract: |t| t.apu_skin_temp_value_c,
    },
    PtsMetricSpec {
        slug: "dgpu-skin-temp",
        title: "dGPU Skin Temp",
        unit: "Celsius",
        proportion: "LIB",
        precision: 2,
        extract: |t| t.dgpu_skin_temp_value_c,
    },
    PtsMetricSpec {
        slug: "fclk",
        title: "Fabric Clock (FCLK)",
        unit: "MHz",
        proportion: "HIB",
        precision: 2,
        extract: |t| t.fclk_mhz,
    },
    PtsMetricSpec {
        slug: "uclk",
        title: "Memory Controller Clock (UCLK)",
        unit: "MHz",
        proportion: "HIB",
        precision: 2,
        extract: |t| t.uclk_mhz,
    },
    PtsMetricSpec {
        slug: "mclk",
        title: "Memory Clock (MCLK)",
        unit: "MHz",
        proportion: "HIB",
        precision: 2,
        extract: |t| t.mclk_mhz,
    },
    PtsMetricSpec {
        slug: "vddcr-soc",
        title: "VDDCR_SOC Voltage",
        unit: "Volts",
        proportion: "LIB",
        precision: 4,
        extract: |t| t.vddcr_soc_v,
    },
];

/// Render a Phoronix Test Suite `composite.xml` document from recorded telemetry samples.
pub fn render_pts_composite_xml(
    title: &str,
    identifier: &str,
    codename: Codename,
    samples: &[RecordedSample],
) -> String {
    let sys = SystemBoardInfo::detect_linux();
    render_pts_composite_xml_with_system(title, identifier, codename, samples, &sys)
}

fn render_pts_composite_xml_with_system(
    title: &str,
    identifier: &str,
    codename: Codename,
    samples: &[RecordedSample],
    sys: &SystemBoardInfo,
) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let last_modified_secs = samples
        .last()
        .map(|s| s.timestamp_unix_ms / 1_000)
        .unwrap_or_else(|| now_unix_ms() / 1_000);
    let last_modified = format_pts_datetime_utc(last_modified_secs);

    let description = format!(
        "zensmu telemetry recording on {} ({} samples)",
        codename.display_name(),
        samples.len()
    );

    let mut hardware = format!("Processor: {}", codename.display_name());
    let board = match (&sys.board_vendor, &sys.board_name) {
        (Some(v), Some(n)) => Some(format!("{v} {n}")),
        (None, Some(n)) => Some(n.clone()),
        (Some(v), None) => Some(v.clone()),
        (None, None) => None,
    };
    if let Some(board_str) = board {
        let _ = write!(hardware, ", Motherboard: {board_str}");
    }
    if let Some(ref bios) = sys.bios_version {
        let _ = write!(hardware, ", BIOS: {bios}");
        if let Some(ref bios_date) = sys.bios_date {
            let _ = write!(hardware, " ({bios_date})");
        }
    }

    let os_name = sys.os_pretty_name.as_deref().unwrap_or("Linux");
    let kernel = sys.kernel_release.as_deref().unwrap_or("unknown");
    let software = format!("OS: {os_name}, Kernel: {kernel}");

    let arguments = samples
        .iter()
        .find_map(|s| s.tag.as_deref().filter(|t| !t.is_empty()))
        .unwrap_or("live-telemetry");

    let escaped_title = xml_escape(title);
    let escaped_identifier = xml_escape(identifier);
    let escaped_description = xml_escape(&description);
    let escaped_hardware = xml_escape(&hardware);
    let escaped_software = xml_escape(&software);
    let escaped_arguments = xml_escape(arguments);
    let escaped_version = xml_escape(version);

    let mut xml = String::with_capacity(2048);
    xml.push_str("<?xml version=\"1.0\"?>\n");
    xml.push_str("<PhoronixTestSuite>\n");
    xml.push_str("  <Generated>\n");
    let _ = writeln!(xml, "    <Title>{escaped_title}</Title>");
    let _ = writeln!(xml, "    <LastModified>{last_modified}</LastModified>");
    let _ = writeln!(
        xml,
        "    <TestClient>zensmu v{escaped_version}</TestClient>"
    );
    let _ = writeln!(xml, "    <Description>{escaped_description}</Description>");
    xml.push_str("  </Generated>\n");
    xml.push_str("  <System>\n");
    let _ = writeln!(xml, "    <Identifier>{escaped_identifier}</Identifier>");
    let _ = writeln!(xml, "    <Hardware>{escaped_hardware}</Hardware>");
    let _ = writeln!(xml, "    <Software>{escaped_software}</Software>");
    xml.push_str("  </System>\n");

    let mut values: Vec<f32> = Vec::new();
    for metric in PTS_METRICS {
        values.clear();
        for sample in samples {
            if let Some(v) = (metric.extract)(&sample.telemetry).filter(|v| v.is_finite()) {
                values.push(v);
            }
        }
        if values.is_empty() {
            continue;
        }

        let mut sum = 0.0_f64;
        let mut min_v = f64::INFINITY;
        let mut max_v = f64::NEG_INFINITY;
        for &v in &values {
            let vf = f64::from(v);
            sum += vf;
            if vf < min_v {
                min_v = vf;
            }
            if vf > max_v {
                max_v = vf;
            }
        }
        let mean = sum / (values.len() as f64);
        let prec = metric.precision;

        let mut raw_series = String::with_capacity(values.len() * 8);
        for (i, &v) in values.iter().enumerate() {
            if i > 0 {
                raw_series.push(':');
            }
            let _ = write!(raw_series, "{v:.prec$}");
        }

        let json_summary = format!(
            "{{\"min\":{min_v:.prec$},\"max\":{max_v:.prec$},\"avg\":{mean:.prec$},\"samples\":{}}}",
            values.len()
        );

        xml.push_str("  <Result>\n");
        let _ = writeln!(xml, "    <Identifier>zensmu/{}</Identifier>", metric.slug);
        let _ = writeln!(xml, "    <Title>{}</Title>", xml_escape(metric.title));
        let _ = writeln!(xml, "    <AppVersion>{escaped_version}</AppVersion>");
        let _ = writeln!(xml, "    <Arguments>{escaped_arguments}</Arguments>");
        let _ = writeln!(
            xml,
            "    <Description>Recorded over {} samples</Description>",
            values.len()
        );
        let _ = writeln!(xml, "    <Scale>{}</Scale>", xml_escape(metric.unit));
        let _ = writeln!(xml, "    <Proportion>{}</Proportion>", metric.proportion);
        xml.push_str("    <DisplayFormat>LINE_GRAPH</DisplayFormat>\n");
        xml.push_str("    <Data>\n");
        xml.push_str("      <Entry>\n");
        let _ = writeln!(xml, "        <Identifier>{escaped_identifier}</Identifier>");
        let _ = writeln!(xml, "        <Value>{mean:.prec$}</Value>");
        let _ = writeln!(xml, "        <RawString>{raw_series}</RawString>");
        let _ = writeln!(xml, "        <JSON>{json_summary}</JSON>");
        xml.push_str("      </Entry>\n");
        xml.push_str("    </Data>\n");
        xml.push_str("  </Result>\n");
    }

    xml.push_str("</PhoronixTestSuite>\n");
    xml
}

/// Convert an existing `zensmu` CSV or JSONL recording file into a Phoronix Test Suite
/// `composite.xml` file, returning the number of converted samples.
pub fn convert_jsonl_or_csv_to_pts(
    input: &Path,
    output: &Path,
    title: Option<&str>,
    identifier: Option<&str>,
) -> Result<usize> {
    let content = fs::read_to_string(input).map_err(|e| map_io_err(input, e))?;
    let first_non_empty = content.lines().map(str::trim).find(|l| !l.is_empty());

    let is_jsonl = RecordFormat::from_path(input) == RecordFormat::Jsonl
        || first_non_empty.is_some_and(|line| line.starts_with('{'));

    let samples = if is_jsonl {
        parse_jsonl_samples(&content)?
    } else {
        parse_csv_samples(&content)?
    };

    if samples.is_empty() {
        return Err(Error::InvalidInput(format!(
            "no telemetry samples found in {}",
            input.display()
        )));
    }

    let codename = samples
        .first()
        .map(|s| s.telemetry.codename)
        .unwrap_or(Codename::Undefined);

    let resolved_title = title
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("zensmu telemetry - {}", derive_pts_label_from_path(input)));

    let resolved_identifier = identifier
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            samples
                .iter()
                .find_map(|s| s.tag.as_deref().filter(|t| !t.is_empty()))
                .map(str::to_string)
        })
        .unwrap_or_else(|| codename.slug().to_string());

    let xml = render_pts_composite_xml(&resolved_title, &resolved_identifier, codename, &samples);
    write_atomic(output, xml.as_bytes())?;
    Ok(samples.len())
}

fn parse_jsonl_samples(content: &str) -> Result<Vec<RecordedSample>> {
    let mut samples = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let sample: RecordedSample = serde_json::from_str(trimmed).map_err(|e| {
            Error::Serde(format!(
                "failed to parse JSONL telemetry sample on line {}: {e}",
                idx + 1
            ))
        })?;
        samples.push(sample);
    }
    Ok(samples)
}
