mod helpers;
mod persist;
mod render;
mod tuning;

use ratatui::backend::TestBackend;
use ratatui::Terminal;
use zensmu_core::hw::identity::HardwareContext;
use zensmu_core::hw::profiles::capabilities_for;
use zensmu_core::hw::topology::CpuTopology;
use zensmu_core::transport::mock::MockTransport;
use zensmu_core::transport::SmuTransport;
use zensmu_core::types::Codename;

pub(super) fn mock_9700x() -> (MockTransport, HardwareContext) {
    let transport = MockTransport::granite_ridge_9700x();
    let driver = transport.read_info().unwrap();
    let hw = HardwareContext {
        driver,
        codename: Codename::GraniteRidge,
        cpuid: None,
        topology: CpuTopology::synthetic(1, 8, 2),
        capabilities: capabilities_for(Codename::GraniteRidge).unwrap(),
    };
    (transport, hw)
}

pub(super) fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
    let buf = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}
