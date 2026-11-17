# sacdfs for Fedora and RHEL (#460), built offline from the release's source and its vendored
# crates with the distribution's Rust: plain cargo, no Rust packaging macros. The files are laid
# out as in the release's .rpm (packaging/nfpm.yaml). packaging/make-srpm.sh fills in the
# version and builds the SRPM; the release workflow attaches it to the release.

# The release binary is stripped; no debuginfo package.
%global debug_package %{nil}
# The release's version (0.7.0, 0.7.0-rc.1); Version has ~ for - (0.7.0~rc.1).
%global upstream_version @UPSTREAM@

Name:           sacdfs
Version:        @RPMVERSION@
Release:        1%{?dist}
Summary:        Read-only FUSE filesystem that shows SACD disc images as DSF files
# sacdfs itself; the vendored crates are under their own licences (vendor/*/).
License:        GPL-3.0-or-later
URL:            https://github.com/aclater/sacdfs
# git archive --prefix=sacdfs-VERSION/ vVERSION
Source0:        %{url}/archive/v%{upstream_version}/%{name}-%{upstream_version}.tar.gz
# cargo vendor --locked --versioned-dirs, of Source0's Cargo.lock
Source1:        %{name}-%{upstream_version}-vendor.tar.xz
# packaging/package-files.sh and the scriptlets' warning, from main
Source2:        package-files.sh

ExclusiveArch:  x86_64 aarch64
BuildRequires:  cargo >= 1.85
BuildRequires:  rust >= 1.85
BuildRequires:  gcc
BuildRequires:  systemd-rpm-macros
Requires:       fuse3

%description
sacdfs mounts a folder of SACD disc images (ISO) and shows each disc's tracks as DSF and
WAV files, decoded on demand. It writes nothing to the images.

%prep
%autosetup -n %{name}-%{upstream_version}
tar -xJf %{SOURCE1}
mkdir -p .cargo
cat > .cargo/config.toml <<'CFG'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"

[net]
offline = true
CFG

%build
cargo build --release --offline --locked --bin sacdfs
bash %{SOURCE2} rpm deploy/sacdfs-ctl deploy/systemd/sacdfs.service deploy/sacdfs.conf.example pkgfiles

%check
target/release/sacdfs --version | grep -qx "sacdfs %{upstream_version}"

%install
install -Dpm 0755 target/release/sacdfs %{buildroot}%{_bindir}/sacdfs
install -Dpm 0755 pkgfiles/sacdfs-ctl %{buildroot}%{_bindir}/sacdfs-ctl
install -Dpm 0644 pkgfiles/sacdfs.service %{buildroot}%{_unitdir}/sacdfs.service
install -Dpm 0644 pkgfiles/sacdfs.conf %{buildroot}%{_sysconfdir}/sysconfig/sacdfs

%post
%systemd_post sacdfs.service
[ -e /usr/local/bin/sacdfs ] &&
    echo "sacdfs: warning: /usr/local/bin/sacdfs (from install.sh) is still there; the package's program is /usr/bin/sacdfs" >&2
[ -e /etc/systemd/system/sacdfs.service ] &&
    echo "sacdfs: warning: /etc/systemd/system/sacdfs.service (from install.sh) overrides the package's unit; remove it and run systemctl daemon-reload to use the package's" >&2
[ -e /etc/default/sacdfs ] &&
    echo "sacdfs: warning: the package's unit and sacdfs-ctl read /etc/sysconfig/sacdfs, not /etc/default/sacdfs (from install.sh)" >&2
:

%preun
%systemd_preun sacdfs.service

%postun
%systemd_postun_with_restart sacdfs.service
# The service's seek index cache (CacheDirectory=sacdfs), on erase only.
[ "$1" -eq 0 ] && rm -rf /var/cache/sacdfs
:

%files
%license COPYING
%doc README.md CHANGELOG.md
%{_bindir}/sacdfs
%{_bindir}/sacdfs-ctl
%{_unitdir}/sacdfs.service
%config(noreplace) %{_sysconfdir}/sysconfig/sacdfs

%changelog
* @DATE@ aclater <105859+aclater@users.noreply.github.com> - @RPMVERSION@-1
- sacdfs @UPSTREAM@: see CHANGELOG.md
