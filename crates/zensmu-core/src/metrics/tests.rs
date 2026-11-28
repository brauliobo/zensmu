use super::*;
use crate::hw::identity::{CpuidInfo, HardwareContext};
use crate::hw::profiles::capabilities_for;
use crate::hw::topology::CpuTopology;
use crate::ops::pm_table;
use crate::transport::mock::MockTransport;
use crate::transport::SmuTransport;
use crate::types::Codename;
use std::collections::HashSet;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

fn granite_ridge_hw(transport: &MockTransport) -> HardwareContext {
    HardwareContext {
        driver: transport.read_info().unwrap(),
        codename: Codename::GraniteRidge,
        cpuid: Some(CpuidInfo {
            vendor: "AuthenticAMD".into(),
            brand: "AMD Ryzen 7 9700X 8-Core Processor".into(),
            family: 0x1A,
            model: 0x44,
            stepping: 0,
        }),
        topology: CpuTopology::synthetic(1, 8, 2),
        capabilities: capabilities_for(Codename::GraniteRidge).unwrap(),
    }
}

fn phoenix_hw(transport: &MockTransport) -> HardwareContext {
    HardwareContext {
        driver: transport.read_info().unwrap(),
        codename: Codename::Phoenix,
        cpuid: Some(CpuidInfo {
            vendor: "AuthenticAMD".into(),
            brand: "AMD Ryzen 7 7840U w/ Radeon 780M Graphics".into(),
            family: 0x19,
            model: 0x74,
            stepping: 1,
        }),
        topology: CpuTopology::synthetic(1, 8, 2),
        capabilities: capabilities_for(Codename::Phoenix).unwrap(),
    }
}

fn assert_no_duplicate_metric_headers(text: &str) {
    let mut seen_help = HashSet::new();
    let mut seen_type = HashSet::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# HELP ") {
            let name = rest.split_whitespace().next().unwrap();
            assert!(
                seen_help.insert(name.to_string()),
                "duplicate # HELP for metric {name}"
            );
        } else if let Some(rest) = line.strip_prefix("# TYPE ") {
            let name = rest.split_whitespace().next().unwrap();
            assert!(
                seen_type.insert(name.to_string()),
                "duplicate # TYPE for metric {name}"
            );
        }
    }
}

fn http_request(addr: SocketAddr, req: &str) -> String {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(req.as_bytes()).unwrap();
    stream.flush().unwrap();
    let mut resp = String::new();
    stream.read_to_string(&mut resp).unwrap();
    resp
}

#[test]
fn renders_granite_ridge_prometheus_metrics() {
    let transport = MockTransport::granite_ridge_9700x();
    let hw = granite_ridge_hw(&transport);
    let snap = pm_table::read_telemetry(&transport, &hw).unwrap();

    let body = render_prometheus_metrics(&hw, &snap, true);
    assert_no_duplicate_metric_headers(&body);

    assert!(body.contains(
        "zensmu_info{codename=\"granite-ridge\",drv_version=\"0.1.7\",smu_version=\"98.79.0\",cpu_name=\"AMD Ryzen 7 9700X 8-Core Processor\"} 1"
    ));
    assert!(body.contains("# TYPE zensmu_ppt_watts gauge\nzensmu_ppt_watts 42.5\n"));
    assert!(body.contains("zensmu_ppt_limit_watts 88\n"));
    assert!(body.contains("zensmu_tdc_amperes 21\n"));
    assert!(body.contains("zensmu_tdc_limit_amperes 75\n"));
    assert!(body.contains("zensmu_edc_amperes 38\n"));
    assert!(body.contains("zensmu_edc_limit_amperes 150\n"));
    assert!(body.contains("zensmu_temperature_celsius{sensor=\"tctl\"} 54.2\n"));
    assert!(body.contains("zensmu_temperature_limit_celsius{sensor=\"tctl\"} 95\n"));
    assert!(body.contains("zensmu_voltage_volts{rail=\"vsoc\"} 1.24\n"));
    assert!(body.contains("zensmu_voltage_volts{rail=\"vddcr_misc\"} 1.1\n"));
    assert!(body.contains("zensmu_voltage_volts{rail=\"cldo_vddp\"} 1.05\n"));
    assert!(body.contains("zensmu_clock_mhz{domain=\"fclk\"} 2000\n"));
    assert!(body.contains("zensmu_clock_mhz{domain=\"uclk\"} 3000\n"));
    assert!(body.contains("zensmu_clock_mhz{domain=\"mclk\"} 3000\n"));

    // APU-only metrics should not appear on desktop telemetry
    assert!(!body.contains("zensmu_stapm_watts"));
    assert!(!body.contains("zensmu_fast_limit_watts"));
    assert!(!body.contains("zensmu_slow_limit_watts"));
    assert!(!body.contains("zensmu_soc_tdc_amperes"));
}

#[test]
fn renders_phoenix_apu_metrics_under_single_headers() {
    let transport = MockTransport::phoenix_apu();
    let hw = phoenix_hw(&transport);
    let snap = pm_table::read_telemetry(&transport, &hw).unwrap();

    let body = render_prometheus_metrics(&hw, &snap, true);
    assert_no_duplicate_metric_headers(&body);

    assert!(body.contains("zensmu_stapm_watts 15\n"));
    assert!(body.contains("zensmu_stapm_limit_watts 28\n"));
    assert!(body.contains("zensmu_fast_limit_watts 45\n"));
    assert!(body.contains("zensmu_slow_limit_watts 35\n"));
    assert!(body.contains("zensmu_soc_tdc_amperes 8\n"));
    assert!(body.contains("zensmu_soc_tdc_limit_amperes 20\n"));
    assert!(body.contains("zensmu_soc_edc_amperes 12\n"));
    assert!(body.contains("zensmu_soc_edc_limit_amperes 35\n"));
    assert!(body.contains("zensmu_temperature_celsius{sensor=\"tctl\"} 52\n"));
    assert!(body.contains("zensmu_temperature_celsius{sensor=\"apu_skin\"} 42\n"));
    assert!(body.contains("zensmu_temperature_celsius{sensor=\"dgpu_skin\"} 40\n"));
}

#[test]
fn escapes_labels_and_omits_none_and_non_finite_values() {
    assert_eq!(
        escape_prom_label("AMD \"Zen\" \\ line1\r\nline2"),
        "AMD \\\"Zen\\\" \\\\ line1\\nline2"
    );

    let transport = MockTransport::granite_ridge_9700x();
    let mut hw = granite_ridge_hw(&transport);
    hw.cpuid.as_mut().unwrap().brand = "AMD \"Zen5\" \\ Rev\nA".to_string();

    let mut snap = pm_table::read_telemetry(&transport, &hw).unwrap();
    snap.ppt_value_w = Some(f32::NAN);
    snap.ppt_limit_w = Some(f32::INFINITY);
    snap.tdc_value_a = Some(f32::NEG_INFINITY);
    snap.tdc_limit_a = None;
    snap.edc_value_a = Some(-0.0);

    let body = render_prometheus_metrics(&hw, &snap, true);
    assert_no_duplicate_metric_headers(&body);

    assert!(body.contains("cpu_name=\"AMD \\\"Zen5\\\" \\\\ Rev\\nA\""));
    assert!(!body.contains("zensmu_ppt_watts"));
    assert!(!body.contains("zensmu_ppt_limit_watts"));
    assert!(!body.contains("zensmu_tdc_amperes"));
    assert!(!body.contains("zensmu_tdc_limit_amperes"));
    assert!(body.contains("zensmu_edc_amperes 0\n"));
    for line in body
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let val_str = line.rsplit(' ').next().unwrap();
        assert_ne!(val_str, "-0", "negative zero emitted in line: {line}");
        let parsed: f64 = val_str
            .parse()
            .unwrap_or_else(|e| panic!("non-numeric sample value '{val_str}' in '{line}': {e}"));
        assert!(
            parsed.is_finite(),
            "non-finite sample value '{val_str}' in '{line}'"
        );
    }
}

#[test]
fn renders_extended_and_toggles_per_core_metrics() {
    let transport = MockTransport::granite_ridge_9700x();
    let hw = granite_ridge_hw(&transport);
    let snap = pm_table::read_telemetry(&transport, &hw).unwrap();

    let extended = ExtendedTelemetry {
        package_power_w: Some(64.5),
        core_power_w: Some(41.25),
        soc_power_w: Some(14.5),
        vcore_v: Some(1.18),
        igpu_clock_mhz: Some(2200.0),
        igpu_temp_c: Some(46.0),
        igpu_power_w: Some(6.5),
        fast_limit_w: None,
        cores: vec![
            CoreMetricsSample {
                core: 0,
                clock_mhz: Some(5450.0),
                effective_clock_mhz: Some(5410.0),
                temperature_c: Some(61.5),
                voltage_v: Some(1.22),
                power_w: Some(9.5),
                c0_percent: Some(98.5),
            },
            CoreMetricsSample {
                core: 1,
                clock_mhz: Some(f32::NAN),
                effective_clock_mhz: Some(4800.0),
                temperature_c: None,
                voltage_v: Some(f32::INFINITY),
                power_w: Some(4.25),
                c0_percent: Some(32.0),
            },
        ],
    };

    let with_cores = render_prometheus_metrics_with_extended(&hw, &snap, Some(&extended), true);
    assert_no_duplicate_metric_headers(&with_cores);
    assert!(with_cores.contains("zensmu_package_power_watts 64.5\n"));
    assert!(with_cores.contains("zensmu_core_power_watts 41.25\n"));
    assert!(with_cores.contains("zensmu_soc_power_watts 14.5\n"));
    assert!(with_cores.contains("zensmu_voltage_volts{rail=\"vcore\"} 1.18\n"));
    assert!(with_cores.contains("zensmu_clock_mhz{domain=\"igpu\"} 2200\n"));
    assert!(with_cores.contains("zensmu_temperature_celsius{sensor=\"igpu\"} 46\n"));
    assert!(with_cores.contains("zensmu_igpu_power_watts 6.5\n"));
    assert!(with_cores.contains("zensmu_core_clock_mhz{core=\"0\"} 5450\n"));
    assert!(!with_cores.contains("zensmu_core_clock_mhz{core=\"1\"}"));
    assert!(with_cores.contains("zensmu_core_effective_clock_mhz{core=\"0\"} 5410\n"));
    assert!(with_cores.contains("zensmu_core_effective_clock_mhz{core=\"1\"} 4800\n"));
    assert!(with_cores.contains("zensmu_core_temperature_celsius{core=\"0\"} 61.5\n"));
    assert!(with_cores.contains("zensmu_core_voltage_volts{core=\"0\"} 1.22\n"));
    assert!(!with_cores.contains("zensmu_core_voltage_volts{core=\"1\"}"));
    assert!(with_cores.contains("zensmu_core_power_watts{core=\"0\"} 9.5\n"));
    assert!(with_cores.contains("zensmu_core_power_watts{core=\"1\"} 4.25\n"));
    assert!(with_cores.contains("zensmu_core_c0_percent{core=\"0\"} 98.5\n"));
    assert!(with_cores.contains("zensmu_core_c0_percent{core=\"1\"} 32\n"));

    let without_cores = render_prometheus_metrics_with_extended(&hw, &snap, Some(&extended), false);
    assert_no_duplicate_metric_headers(&without_cores);
    assert!(without_cores.contains("zensmu_core_power_watts 41.25\n"));
    assert!(!without_cores.contains("{core="));
    assert!(!without_cores.contains("zensmu_core_clock_mhz"));
    assert!(!without_cores.contains("zensmu_core_effective_clock_mhz"));
    assert!(!without_cores.contains("zensmu_core_temperature_celsius"));
    assert!(!without_cores.contains("zensmu_core_voltage_volts"));
    assert!(!without_cores.contains("zensmu_core_c0_percent"));
}

#[test]
fn serves_metrics_over_loopback_and_exits_cleanly_on_stop_flag() {
    let transport = Arc::new(MockTransport::granite_ridge_9700x());
    let hw = granite_ridge_hw(&transport);
    let stop_flag = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();

    let srv_transport = Arc::clone(&transport);
    let srv_stop = Arc::clone(&stop_flag);
    let handle = thread::spawn(move || {
        let cfg = MetricsServerConfig {
            listen_addr: "127.0.0.1:0".to_string(),
            include_per_core: true,
        };
        serve_metrics(srv_transport.as_ref(), &hw, &cfg, &srv_stop, |addr| {
            let _ = tx.send(addr);
        })
    });

    let addr = rx.recv_timeout(Duration::from_secs(5)).unwrap();

    // GET /metrics
    let metrics_resp = http_request(addr, "GET /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert!(metrics_resp.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(metrics_resp.contains("Content-Type: text/plain; version=0.0.4; charset=utf-8\r\n"));
    assert!(metrics_resp.contains("Connection: close\r\n"));
    assert!(metrics_resp.contains("zensmu_info{codename=\"granite-ridge\""));
    assert!(metrics_resp.contains("zensmu_ppt_watts 42.5\n"));

    // HEAD /metrics (headers only, no body)
    let head_resp = http_request(addr, "HEAD /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert!(head_resp.starts_with("HTTP/1.1 200 OK\r\n"));
    let (_, head_body) = head_resp.split_once("\r\n\r\n").unwrap();
    assert!(head_body.is_empty());

    // GET /healthz
    let health_resp = http_request(addr, "GET /healthz HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert!(health_resp.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(health_resp.ends_with("\r\n\r\nok\n"));

    // GET /
    let index_resp = http_request(addr, "GET / HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert!(index_resp.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(index_resp.contains("href=\"/metrics\""));

    // GET /missing -> 404
    let missing_resp = http_request(addr, "GET /missing HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert!(missing_resp.starts_with("HTTP/1.1 404 Not Found\r\n"));

    // POST /metrics -> 405
    let method_resp = http_request(addr, "POST /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert!(method_resp.starts_with("HTTP/1.1 405 Method Not Allowed\r\n"));
    assert!(method_resp.contains("Allow: GET, HEAD\r\n"));

    stop_flag.store(true, Ordering::Relaxed);
    let result = handle.join().unwrap();
    assert!(result.is_ok());
}

#[test]
fn returns_503_when_telemetry_is_unavailable() {
    let transport = Arc::new(MockTransport::granite_ridge_9700x());
    let mut hw = granite_ridge_hw(&transport);
    hw.driver.has_pm_table = false;
    let stop_flag = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();

    let srv_transport = Arc::clone(&transport);
    let srv_stop = Arc::clone(&stop_flag);
    let handle = thread::spawn(move || {
        let cfg = MetricsServerConfig {
            listen_addr: "127.0.0.1:0".to_string(),
            include_per_core: false,
        };
        serve_metrics(srv_transport.as_ref(), &hw, &cfg, &srv_stop, |addr| {
            let _ = tx.send(addr);
        })
    });

    let addr = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let resp = http_request(addr, "GET /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert!(resp.starts_with("HTTP/1.1 503 Service Unavailable\r\n"));
    assert!(resp.contains("telemetry unavailable:"));

    stop_flag.store(true, Ordering::Relaxed);
    assert!(handle.join().unwrap().is_ok());
}

#[test]
fn config_default_and_service_status_helpers_are_well_formed() {
    let cfg = MetricsServerConfig::default();
    assert_eq!(cfg.listen_addr, DEFAULT_METRICS_LISTEN);
    assert!(cfg.include_per_core);

    let status = read_exporter_service_status();
    assert_eq!(status.service_name, USER_EXPORTER_SERVICE_NAME);
    assert_eq!(status.endpoint, "http://127.0.0.1:9452/metrics");
    assert!(!status.state.is_empty());
}
