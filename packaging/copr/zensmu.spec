# Cargo release profile does not emit DWARF debuginfo by default.
%global debug_package %{nil}

Name:           zensmu
Version:        0.3.0
Release:        1%{?dist}
Summary:        Safety-first Rust CLI, live TUI, and unprivileged GUI for AMD Ryzen SMU control and telemetry

License:        MIT
URL:            https://github.com/brauliobo/zensmu
Source0:        %{url}/archive/refs/tags/v%{version}.tar.gz

ExclusiveArch:  x86_64

BuildRequires:  cargo
BuildRequires:  rust >= 1.78
BuildRequires:  pkgconfig
BuildRequires:  libxkbcommon-devel
BuildRequires:  wayland-devel
BuildRequires:  libxcb-devel
BuildRequires:  mesa-libGL-devel
BuildRequires:  systemd-rpm-macros
BuildRequires:  desktop-file-utils

Requires:       polkit
Recommends:     libglvnd-glx
Recommends:     libxkbcommon
Recommends:     libwayland-client
Recommends:     libxcb
Suggests:       ryzen-smu-dkms

%description
Safety-first Rust CLI (zensmu), live TUI (zensmu top), unprivileged desktop GUI
(zensmu-gui), and Prometheus telemetry exporter for AMD Ryzen SMU control,
Curve Optimizer, Curve Shaper, PBO/APU limits, and live telemetry on Linux.

%prep
%autosetup -n %{name}-%{version}

%build
cargo build --release --workspace --locked

%install
# Binaries
install -Dm755 target/release/zensmu %{buildroot}%{_bindir}/zensmu
install -Dm755 target/release/zensmu-gui %{buildroot}%{_bindir}/zensmu-gui

# Systemd units and suspend/hibernate resume hook
install -Dm644 packaging/systemd/zensmu-apply.service \
    %{buildroot}%{_unitdir}/zensmu-apply.service
install -Dm644 packaging/systemd/zensmu-record.service \
    %{buildroot}%{_userunitdir}/zensmu-record.service
install -Dm644 packaging/systemd/zensmu-exporter.service \
    %{buildroot}%{_userunitdir}/zensmu-exporter.service
install -Dm755 packaging/systemd/zensmu-resume \
    %{buildroot}%{_prefix}/lib/systemd/system-sleep/zensmu-resume

# PolicyKit policy for unprivileged zensmu-gui elevation
install -Dm644 packaging/polkit/org.zensmu.policy \
    %{buildroot}%{_datadir}/polkit-1/actions/org.zensmu.policy

# Desktop launcher and SVG application icon
install -Dm644 packaging/desktop/zensmu-gui.desktop \
    %{buildroot}%{_datadir}/applications/zensmu-gui.desktop
install -Dm644 packaging/desktop/zensmu.svg \
    %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/zensmu.svg

# Profile configuration directory
install -dm755 %{buildroot}%{_sysconfdir}/zensmu

# Shell completions
install -dm755 \
    %{buildroot}%{_datadir}/bash-completion/completions \
    %{buildroot}%{_datadir}/zsh/site-functions \
    %{buildroot}%{_datadir}/fish/vendor_completions.d
./target/release/zensmu completions bash > %{buildroot}%{_datadir}/bash-completion/completions/zensmu
./target/release/zensmu completions zsh  > %{buildroot}%{_datadir}/zsh/site-functions/_zensmu
./target/release/zensmu completions fish > %{buildroot}%{_datadir}/fish/vendor_completions.d/zensmu.fish

# Man page
install -dm755 %{buildroot}%{_mandir}/man1
if [ -f packaging/man/zensmu.1 ]; then
    install -Dm644 packaging/man/zensmu.1 %{buildroot}%{_mandir}/man1/zensmu.1
else
    ./target/release/zensmu manpage > %{buildroot}%{_mandir}/man1/zensmu.1
fi

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/zensmu-gui.desktop

%post
%systemd_post zensmu-apply.service
%systemd_user_post zensmu-record.service zensmu-exporter.service

%preun
%systemd_preun zensmu-apply.service
%systemd_user_preun zensmu-record.service zensmu-exporter.service

%postun
%systemd_postun zensmu-apply.service
%systemd_user_postun zensmu-record.service zensmu-exporter.service

%files
%license LICENSE
%doc README.md
%{_bindir}/zensmu
%{_bindir}/zensmu-gui
%{_unitdir}/zensmu-apply.service
%{_userunitdir}/zensmu-record.service
%{_userunitdir}/zensmu-exporter.service
%{_prefix}/lib/systemd/system-sleep/zensmu-resume
%{_datadir}/polkit-1/actions/org.zensmu.policy
%{_datadir}/applications/zensmu-gui.desktop
%{_datadir}/icons/hicolor/scalable/apps/zensmu.svg
%dir %{_sysconfdir}/zensmu
%{_datadir}/bash-completion/completions/zensmu
%{_datadir}/zsh/site-functions/_zensmu
%{_datadir}/fish/vendor_completions.d/zensmu.fish
%{_mandir}/man1/zensmu.1*

%changelog
* Thu Nov 26 2026 Braulio Oliveira <brauliobo@gmail.com> - 0.3.0-1
- Initial Fedora COPR RPM package for zensmu 0.3.0
