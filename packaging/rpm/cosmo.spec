# cosmo for Fedora, built in COPR (phase-8 §8.3).
#
# The COPR project needs networking enabled for builds ("enable_net"):
# scripts/fetch-native downloads and builds sherpa-onnx (and the static
# ONNX Runtime it links), cargo fetches crates by Cargo.lock, and
# cargo-about is installed into the build root to write the licence
# notices. Each download is pinned by checksum or by Cargo.lock.
#
# Models are never packaged: each user runs `cosmo models fetch`.

%global debug_package %{nil}
%global appid io.github.speerdo

Name:           cosmo
Version:        0.1.0
Release:        1%{?dist}
Summary:        Voice front-end for MCP on the COSMIC desktop
# cosmo's own code is MIT. The binaries statically link permissively
# licensed crates and native libraries (THIRD_PARTY.md, and the generated
# third-party-licenses.txt).
License:        MIT AND Apache-2.0 AND BSD-2-Clause AND BSD-3-Clause AND ISC AND Zlib AND MPL-2.0 AND OFL-1.1 AND Unicode-3.0
URL:            https://github.com/speerdo/cosmic-ai-assistant
Source0:        %{url}/archive/v%{version}/cosmic-ai-assistant-%{version}.tar.gz
ExclusiveArch:  x86_64

# rust-toolchain.toml pins 1.94; the distro's must be at least that.
BuildRequires:  cargo >= 1.94
BuildRequires:  rust >= 1.94
BuildRequires:  gcc
BuildRequires:  gcc-c++
BuildRequires:  make
BuildRequires:  cmake
BuildRequires:  clang
BuildRequires:  clang-devel
BuildRequires:  curl
BuildRequires:  python3
BuildRequires:  binutils
BuildRequires:  pkgconfig(libpipewire-0.3)
BuildRequires:  pkgconfig(xkbcommon)
BuildRequires:  pkgconfig(wayland-client)
BuildRequires:  wayland-protocols-devel
BuildRequires:  systemd-rpm-macros

# Kokoro's phonemes: the system eSpeak NG, loaded at run time (GPL-3.0,
# never linked or shipped by cosmo).
Requires:       espeak-ng
Requires:       tmux
# `cosmo models fetch`
Requires:       curl
Requires:       bzip2
Requires:       tar
Recommends:     cosmo-applet = %{version}-%{release}
Suggests:       npm

%description
cosmo gives a hard-gated microphone to MCP servers on your Linux desktop,
speaks in a voice you chose, and knows how to drive COSMIC. Hold Right Ctrl
and talk: speech recognition and the voice are local; only the reasoning
model is in the cloud, with the provider of your choice.

After installing, run `cosmo models fetch` once (about 1.6 GB of local
models), `cosmo auth-login --provider openrouter` to sign in with your
browser (other providers take an API key), then `cosmo doctor`. Desktop
control needs computer-use-linux:
npm install -g @agent-sh/computer-use-linux@0.5.0

%package -n cosmo-applet
Summary:        COSMIC panel applet for cosmo
# It links two GPL-3.0-only cosmic-panel crates, as every COSMIC applet
# does; its own source is MIT. Complete source: Source0.
License:        GPL-3.0-only AND MIT AND Apache-2.0 AND MPL-2.0 AND OFL-1.1
Requires:       cosmo = %{version}-%{release}

%description -n cosmo-applet
Shows cosmo's state in the COSMIC panel, with a popup for its voice,
listening and readiness. Add it in Settings, Desktop, Panel, Applets.
A thin client of the cosmo daemon, which owns everything.

%prep
%autosetup -n cosmic-ai-assistant-%{version}

%build
export CARGO_HOME="$PWD/.cargo-home"
scripts/fetch-native
cargo install --locked --root "$PWD/.tools" cargo-about --features cli
cargo build --release --locked -p cosmo-daemon --features ears --bin cosmod
cargo build --release --locked -p cosmo-cli -p cosmo-overlay -p cosmo-applet
packaging/check-deps target/release
PATH="$PWD/.tools/bin:$PATH" packaging/gen-licenses packaging/out/licenses
for p in cosmo cosmo-applet; do
    mkdir -p "licenses-$p"
    cp "packaging/out/licenses/$p.txt" "licenses-$p/third-party-licenses.txt"
done

%install
install -Dm0755 -t %{buildroot}%{_bindir} \
    target/release/cosmod target/release/cosmo target/release/cosmo-overlay \
    target/release/cosmo-applet
install -Dm0755 scripts/fetch-models %{buildroot}%{_libexecdir}/cosmo/fetch-models
install -Dm0644 packaging/systemd/cosmo.service %{buildroot}%{_userunitdir}/cosmo.service
# Enabled for every user (a COPR choice: Fedora proper keeps presets in
# fedora-release). It starts with each user's graphical session.
install -dm0755 %{buildroot}%{_userpresetdir}
echo "enable cosmo.service" >%{buildroot}%{_userpresetdir}/80-cosmo.preset
install -Dm0644 packaging/desktop/%{appid}.CosmoOverlay.desktop \
    %{buildroot}%{_sysconfdir}/xdg/autostart/%{appid}.CosmoOverlay.desktop
install -Dm0644 packaging/desktop/%{appid}.CosmoApplet.desktop \
    %{buildroot}%{_datadir}/applications/%{appid}.CosmoApplet.desktop
install -Dm0644 assets/icons/%{appid}.Cosmo.svg \
    %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/%{appid}.Cosmo.svg

%post
%systemd_user_post cosmo.service

%preun
%systemd_user_preun cosmo.service

%files
%license LICENSE
%license licenses-cosmo/third-party-licenses.txt
%doc README.md THIRD_PARTY.md
%{_bindir}/cosmod
%{_bindir}/cosmo
%{_bindir}/cosmo-overlay
%{_libexecdir}/cosmo/
%{_userunitdir}/cosmo.service
%{_userpresetdir}/80-cosmo.preset
%config(noreplace) %{_sysconfdir}/xdg/autostart/%{appid}.CosmoOverlay.desktop

%files -n cosmo-applet
%license LICENSE
# The GPL-3.0 text is in here, with the cosmic-panel crates that carry it.
%license licenses-cosmo-applet/third-party-licenses.txt
%{_bindir}/cosmo-applet
%{_datadir}/applications/%{appid}.CosmoApplet.desktop
%{_datadir}/icons/hicolor/scalable/apps/%{appid}.Cosmo.svg

%changelog
* Sun Oct 04 2026 speerdo <adamspeer@gmail.com> - 0.1.0-1
- First package: daemon, CLI, overlay, applet; models fetched per user.
