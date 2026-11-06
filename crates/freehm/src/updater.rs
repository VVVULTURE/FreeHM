//! Keeps FreeHM up to date.
//!
//! A few seconds after starting (and whenever the user asks) FreeHM asks GitHub for the
//! latest full release. How a newer one is installed depends on how this copy was installed:
//!
//! - **Windows installer:** a notification, and an Install button. Only after that click is
//!   the installer downloaded, checked against the release's `SHA256SUMS.txt` and run.
//! - **AppImage:** downloaded, checked and swapped in without asking; FreeHM then restarts.
//! - **Anything else** (.deb, .tar.gz, a build from source): a notification with a link.
//!
//! Nothing is installed or restarted while a game is active. No check, or a failed one,
//! changes nothing else: FreeHM works the same without a network.
//!
//! All network traffic is on background threads, which report through [`UpdateEvent`]s.

use crate::app::{AppEvent, EventSink, Platform};
use freehm_core::ipc::{UpdateMethod, UpdatePhase, UpdateStatus};
use freehm_core::update::{self, Asset, CHECKSUMS_FILE, LATEST_RELEASE_API, Release, Version};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// Time for the network to come up after a login.
const FIRST_CHECK: Duration = Duration::from_secs(15);
/// After a failed automatic check: still the check for this start, not a periodic one.
const RETRIES: [Duration; 2] = [Duration::from_secs(60), Duration::from_secs(300)];
/// For the settings window to see the restart coming.
const RESTART_DELAY: Duration = Duration::from_secs(2);
/// A FreeHM release file is a few megabytes.
const MAX_DOWNLOAD: u64 = 256 << 20;
const MAX_TEXT: u64 = 1 << 20;

const APPIMAGE_DOWNLOAD: &str = ".FreeHM-update.AppImage";

pub enum UpdateEvent {
    /// A check finished: the newer release, if there is one.
    Checked(Result<Option<Release>, String>),
    Progress(f32),
    /// Downloaded and verified.
    Downloaded(PathBuf),
    Failed(String),
    /// The installer ended while this process lives on, so it did not replace it.
    InstallerExited(Option<i32>),
}

/// Test hook: look at this address instead of the latest release, trusting what it says.
fn test_url() -> Option<String> {
    std::env::var("FREEHM_UPDATE_URL").ok().filter(|u| !u.is_empty())
}

/// The running version (`FREEHM_UPDATE_CURRENT` lets a test pretend to be older).
fn current_version() -> Option<Version> {
    Version::parse(&std::env::var("FREEHM_UPDATE_CURRENT").unwrap_or_else(|_| freehm_core::APP_VERSION.to_string()))
}

#[cfg(windows)]
fn tls() -> ureq::tls::TlsConfig {
    use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
    TlsConfig::builder().provider(TlsProvider::NativeTls).root_certs(RootCerts::PlatformVerifier).build()
}

#[cfg(not(windows))]
fn tls() -> ureq::tls::TlsConfig {
    use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
    TlsConfig::builder()
        .provider(TlsProvider::Rustls)
        .root_certs(RootCerts::PlatformVerifier)
        .unversioned_rustls_crypto_provider(std::sync::Arc::new(rustls::crypto::ring::default_provider()))
        .build()
}

fn agent(https_only: bool) -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(https_only)
        .user_agent(format!("FreeHM/{}", freehm_core::APP_VERSION))
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_per_read(Some(Duration::from_secs(60)))
        .tls_config(tls())
        .build()
        .new_agent()
}

fn fetch_text(agent: &ureq::Agent, url: &str) -> Result<String, String> {
    let mut r = agent.get(url).header("Accept", "application/vnd.github+json").call().map_err(|e| e.to_string())?;
    r.body_mut().with_config().limit(MAX_TEXT).read_to_string().map_err(|e| e.to_string())
}

/// The newer release, if there is one.
fn check(current: &Version) -> Result<Option<Release>, String> {
    let test = test_url();
    let text = fetch_text(&agent(test.is_none()), test.as_deref().unwrap_or(LATEST_RELEASE_API))?;
    let release = update::parse_release(&text)?;
    if test.is_none() && !release.is_official() {
        return Err(format!("the description of release {} points outside this project", release.version));
    }
    Ok((release.version > *current).then_some(release))
}

/// Fetch `asset` to `dest`, or leave nothing behind. It must match the release's checksum
/// file, fetched first; the partial file never has the final name.
fn download(agent: &ureq::Agent, asset: &Asset, checksums: &Asset, dest: &Path, mut progress: impl FnMut(f32)) -> Result<(), String> {
    let sums = fetch_text(agent, &checksums.url).map_err(|e| format!("could not read {CHECKSUMS_FILE}: {e}"))?;
    let want = update::expected_sha256(&sums, &asset.name).ok_or_else(|| format!("{CHECKSUMS_FILE} does not list {}", asset.name))?;
    let mut resp = agent.get(&asset.url).call().map_err(|e| format!("could not download {}: {e}", asset.name))?;
    let total = resp.body().content_length().filter(|&n| n > 0);
    if total.is_some_and(|n| n > MAX_DOWNLOAD) {
        return Err(format!("{} is larger than any FreeHM release", asset.name));
    }
    let part = dest.with_extension("part");
    let mut reader = resp.body_mut().with_config().limit(MAX_DOWNLOAD).reader();
    let copied = (|| {
        let mut file = std::fs::File::create(&part).map_err(|e| format!("cannot write {}: {e}", part.display()))?;
        let mut hash = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        let (mut done, mut shown) = (0u64, 0u64);
        loop {
            let n = reader.read(&mut buf).map_err(|e| format!("the download of {} stopped: {e}", asset.name))?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
            file.write_all(&buf[..n]).map_err(|e| format!("cannot write {}: {e}", part.display()))?;
            done += n as u64;
            if let Some(t) = total {
                let pct = done * 100 / t;
                if pct != shown {
                    shown = pct;
                    progress(done as f32 / t as f32);
                }
            }
        }
        file.flush().map_err(|e| e.to_string())?;
        let got: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if got == want { Ok(()) } else { Err(format!("{} does not match its published checksum, so it was thrown away", asset.name)) }
    })();
    let moved = copied.and_then(|()| std::fs::rename(&part, dest).map_err(|e| format!("cannot write {}: {e}", dest.display())));
    if moved.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    moved
}

fn updates_dir() -> PathBuf {
    crate::paths::update_dir()
}

/// Downloads finished with: the installer of the last update, an interrupted download.
fn clean_downloads() {
    let _ = std::fs::remove_dir_all(updates_dir());
}

fn appimage_path() -> Option<PathBuf> {
    std::env::var_os("APPIMAGE").filter(|v| !v.is_empty()).map(PathBuf::from)
}

fn detect_method() -> UpdateMethod {
    if cfg!(windows) {
        // Inno Setup's uninstaller sits next to an installed copy.
        match std::env::current_exe() {
            Ok(exe) if exe.with_file_name("unins000.exe").exists() => UpdateMethod::Installer,
            _ => UpdateMethod::Manual,
        }
    } else if appimage_path().is_some_and(|p| p.exists()) {
        UpdateMethod::AppImage
    } else {
        UpdateMethod::Manual
    }
}

/// Was FreeHM installed for all users (the installer's registry entry says so)?
#[cfg(windows)]
fn installed_for_all_users() -> bool {
    use crate::platform::windows::util::wide;
    use windows::Win32::System::Registry::{HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_OPEN_CREATE_OPTIONS, RegCloseKey, RegOpenKeyExW};
    use windows::core::PCWSTR;
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{F0309FCD-477A-4D75-A8ED-61F6BFC20E4C}_is1");
    let mut h = HKEY::default();
    unsafe {
        let found = RegOpenKeyExW(HKEY_LOCAL_MACHINE, PCWSTR(key.as_ptr()), Some(REG_OPEN_CREATE_OPTIONS(0).0), KEY_READ, &mut h).is_ok();
        if found {
            let _ = RegCloseKey(h);
        }
        found
    }
}

#[cfg(not(windows))]
fn installed_for_all_users() -> bool {
    false
}

/// Run the verified installer without its wizard. It stops this process and starts the new
/// version, so an answer only arrives when it did not get that far.
fn run_installer(path: PathBuf, sink: EventSink) {
    let scope = if installed_for_all_users() { "/ALLUSERS" } else { "/CURRENTUSER" };
    let _ = std::thread::Builder::new().name("installer".into()).spawn(move || {
        let args = ["/SILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/freehmrestart=1", scope];
        let event = match Command::new(&path).args(args).status() {
            Ok(s) => UpdateEvent::InstallerExited(s.code()),
            Err(e) => UpdateEvent::Failed(format!("could not start the installer: {e}")),
        };
        sink.send(AppEvent::Update(event));
    });
}

/// Put the verified AppImage in the running one's place. The running copy stays usable: its
/// file lives on until the process ends.
fn replace_appimage(new: &Path, target: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(new, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(new, target)
}

/// Open a web page in the default browser.
pub fn open_url(url: &str) {
    if !url.starts_with("https://") {
        return;
    }
    #[cfg(windows)]
    {
        use crate::platform::windows::util::wide;
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        use windows::core::{PCWSTR, w};
        let u = wide(url);
        unsafe { ShellExecuteW(None, w!("open"), PCWSTR(u.as_ptr()), None, None, SW_SHOWNORMAL) };
    }
    #[cfg(not(windows))]
    let _ = Command::new("xdg-open").arg(url).spawn();
}

/// Wait for the instance being replaced to end (`--restart-after`).
#[cfg(target_os = "linux")]
pub fn wait_for_exit(pid: u32) {
    let proc = PathBuf::from(format!("/proc/{pid}"));
    let start = Instant::now();
    while proc.exists() && start.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub struct Updater {
    sink: EventSink,
    current: Option<Version>,
    status: UpdateStatus,
    release: Option<Release>,
    downloaded: Option<PathBuf>,
    /// When to look next on our own.
    next_check: Option<Instant>,
    /// Failed tries of the check at start; `None` once that check is settled.
    failures: Option<usize>,
    checking: bool,
    /// The user has been told about the current state of the update.
    notified: bool,
    restart_at: Option<Instant>,
}

impl Updater {
    /// `automatic`: look when FreeHM starts (never with simulated data).
    pub fn new(sink: EventSink, automatic: bool) -> Updater {
        clean_downloads();
        let status = UpdateStatus { method: detect_method(), ..Default::default() };
        let next_check = automatic.then(|| Instant::now() + FIRST_CHECK);
        Updater {
            sink,
            current: current_version(),
            status,
            release: None,
            downloaded: None,
            next_check,
            failures: automatic.then_some(0),
            checking: false,
            notified: false,
            restart_at: None,
        }
    }

    pub fn status(&self) -> &UpdateStatus {
        &self.status
    }

    fn busy(&self) -> bool {
        matches!(self.status.phase, UpdatePhase::Downloading | UpdatePhase::WaitingForGame | UpdatePhase::Installing)
    }

    /// Text of the tray menu item, while there is something to click.
    pub fn tray_label(&self) -> Option<String> {
        let v = self.status.latest.as_ref()?;
        (self.status.phase == UpdatePhase::Available).then(|| match self.status.method {
            UpdateMethod::Manual => format!("Get FreeHM {v}"),
            _ => format!("Install FreeHM {v}"),
        })
    }

    pub fn check_now(&mut self) {
        if !self.busy() {
            self.next_check = None;
            self.failures = None;
            self.start_check();
        }
    }

    fn start_check(&mut self) {
        let Some(current) = self.current.clone() else {
            self.status.phase = UpdatePhase::CheckFailed;
            self.status.message = Some("this build's version number could not be read".into());
            return;
        };
        if self.checking {
            return;
        }
        self.checking = true;
        if self.status.phase != UpdatePhase::Available {
            self.status.phase = UpdatePhase::Checking;
        }
        let sink = self.sink.clone();
        let _ = std::thread::Builder::new().name("update-check".into()).spawn(move || {
            sink.send(AppEvent::Update(UpdateEvent::Checked(check(&current))));
        });
    }

    fn checked(&mut self, result: Result<Option<Release>, String>) {
        self.checking = false;
        if self.busy() {
            return;
        }
        if result.is_ok() {
            self.failures = None;
        }
        match result {
            Ok(Some(release)) => {
                log::info!("FreeHM {} is available: {}", release.version, release.page_url);
                self.status.phase = UpdatePhase::Available;
                self.status.latest = Some(release.version.to_string());
                self.status.page_url = Some(release.page_url.clone());
                self.status.message = None;
                self.release = Some(release);
                self.notified = false;
                if self.status.method == UpdateMethod::AppImage {
                    let _ = self.start_download();
                }
            }
            Ok(None) => {
                log::info!("FreeHM is up to date");
                self.status.phase = UpdatePhase::UpToDate;
                self.status.message = None;
            }
            Err(e) => {
                log::info!("update check failed: {e}");
                if self.release.is_none() {
                    self.status.phase = UpdatePhase::CheckFailed;
                    self.status.message = Some(e);
                }
                if let Some(n) = self.failures {
                    self.failures = Some(n + 1);
                    self.next_check = RETRIES.get(n).map(|d| Instant::now() + *d);
                }
            }
        }
    }

    /// The user chose to update, or an AppImage found one.
    pub fn install_now(&mut self) -> Result<(), String> {
        match (self.status.phase, self.status.method) {
            (UpdatePhase::Available, UpdateMethod::Manual) => {
                Err(format!("this copy was not installed by FreeHM's installer; get the new version from {}", self.status.page_url.clone().unwrap_or_default()))
            }
            (UpdatePhase::Available, _) => self.start_download(),
            (UpdatePhase::Downloading | UpdatePhase::WaitingForGame | UpdatePhase::Installing, _) => Ok(()),
            _ => Err("no update has been found".into()),
        }
    }

    fn start_download(&mut self) -> Result<(), String> {
        let prepared = self.prepare_download();
        let (asset, checksums, dest) = match prepared {
            Ok(p) => p,
            Err(e) => {
                self.status.message = Some(e.clone());
                self.notified = false;
                return Err(e);
            }
        };
        self.status.phase = UpdatePhase::Downloading;
        self.status.progress = Some(0.0);
        self.status.message = None;
        let sink = self.sink.clone();
        let https_only = test_url().is_none();
        let _ = std::thread::Builder::new().name("update-download".into()).spawn(move || {
            log::info!("downloading {}", asset.url);
            let progress = |x: f32| sink.send(AppEvent::Update(UpdateEvent::Progress(x)));
            let event = match download(&agent(https_only), &asset, &checksums, &dest, progress) {
                Ok(()) => UpdateEvent::Downloaded(dest),
                Err(e) => UpdateEvent::Failed(e),
            };
            sink.send(AppEvent::Update(event));
        });
        Ok(())
    }

    fn prepare_download(&self) -> Result<(Asset, Asset, PathBuf), String> {
        let release = self.release.as_ref().ok_or("no update has been found")?;
        let asset = match self.status.method {
            UpdateMethod::Installer => release.installer(),
            UpdateMethod::AppImage => release.appimage(),
            UpdateMethod::Manual => None,
        };
        let asset = asset.ok_or_else(|| format!("release {} has no file for this copy of FreeHM", release.version))?;
        let checksums = release.checksums().ok_or_else(|| format!("release {} has no {CHECKSUMS_FILE}", release.version))?;
        let dest = match self.status.method {
            // Next to the AppImage: replacing it must be one rename on one disk.
            UpdateMethod::AppImage => appimage_path().and_then(|p| Some(p.parent()?.join(APPIMAGE_DOWNLOAD))).ok_or("the AppImage's location is unknown")?,
            _ => {
                let name = Path::new(&asset.name).file_name().ok_or("the release file has no name")?;
                let dir = updates_dir();
                std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
                dir.join(name)
            }
        };
        Ok((asset.clone(), checksums.clone(), dest))
    }

    fn failed(&mut self, e: String) {
        log::warn!("update failed: {e}");
        self.status.phase = UpdatePhase::Available;
        self.status.progress = None;
        self.status.message = Some(e);
        self.notified = false;
    }

    pub fn handle(&mut self, ev: UpdateEvent) {
        match ev {
            UpdateEvent::Checked(r) => self.checked(r),
            UpdateEvent::Progress(x) => {
                if self.status.phase == UpdatePhase::Downloading {
                    self.status.progress = Some(x);
                }
            }
            UpdateEvent::Downloaded(path) => {
                log::info!("downloaded and verified {}", path.display());
                self.downloaded = Some(path);
                self.status.phase = UpdatePhase::WaitingForGame;
                self.status.progress = None;
            }
            UpdateEvent::Failed(e) => self.failed(e),
            UpdateEvent::InstallerExited(Some(0)) => log::info!("the installer finished"),
            UpdateEvent::InstallerExited(code) => {
                let code = code.map_or("unknown".to_string(), |c| c.to_string());
                self.failed(format!("the installer stopped without updating FreeHM (exit code {code}); if Windows asked for permission, it needs a Yes"));
            }
        }
    }

    fn notice(&self) -> Option<(String, String)> {
        let version = self.status.latest.as_deref()?;
        let page = self.status.page_url.as_deref().unwrap_or_default();
        Some(match (&self.status.message, self.status.method) {
            (Some(why), UpdateMethod::Manual) => ("FreeHM was not updated".to_string(), format!("{why}\n{page}")),
            (Some(why), _) => ("FreeHM was not updated".to_string(), why.clone()),
            (None, UpdateMethod::Manual) => (format!("FreeHM {version} is available"), format!("Download it from {page}")),
            (None, _) => (format!("FreeHM {version} is available"), "Open FreeHM's settings to install it.".to_string()),
        })
    }

    /// Called once a second and when the game changes. True: quit, the new version is starting.
    pub fn tick(&mut self, game_active: bool, enabled: bool, p: &mut dyn Platform) -> bool {
        if enabled && self.next_check.is_some_and(|t| Instant::now() >= t) {
            self.next_check = None;
            self.start_check();
        }
        if let Some(t) = self.restart_at
            && Instant::now() >= t
        {
            self.restart_at = None;
            return self.restart();
        }
        if game_active {
            return false;
        }
        match self.status.phase {
            UpdatePhase::Available if !self.notified => {
                self.notified = true;
                if let Some((title, text)) = self.notice() {
                    p.notify(&title, &text);
                }
            }
            UpdatePhase::WaitingForGame => self.install(p),
            _ => {}
        }
        false
    }

    fn install(&mut self, p: &mut dyn Platform) {
        let Some(path) = self.downloaded.take() else { return };
        match self.status.method {
            UpdateMethod::Installer => {
                log::info!("running {}", path.display());
                self.status.phase = UpdatePhase::Installing;
                run_installer(path, self.sink.clone());
            }
            UpdateMethod::AppImage => {
                let replaced = appimage_path()
                    .ok_or_else(|| "the AppImage's location is unknown".to_string())
                    .and_then(|target| replace_appimage(&path, &target).map_err(|e| format!("cannot replace {}: {e}", target.display())));
                match replaced {
                    Ok(()) => {
                        let version = self.status.latest.clone().unwrap_or_default();
                        log::info!("AppImage replaced with version {version}");
                        self.status.phase = UpdatePhase::Installing;
                        self.restart_at = Some(Instant::now() + RESTART_DELAY);
                        p.notify("FreeHM was updated", &format!("FreeHM is restarting as version {version}."));
                    }
                    Err(e) => {
                        let _ = std::fs::remove_file(&path);
                        self.failed(e);
                    }
                }
            }
            UpdateMethod::Manual => {}
        }
    }

    /// Start the replaced AppImage; it waits for this process to end.
    fn restart(&mut self) -> bool {
        let spawned = appimage_path()
            .ok_or_else(|| "the AppImage's location is unknown".to_string())
            .and_then(|exe| Command::new(exe).arg("--restart-after").arg(std::process::id().to_string()).spawn().map_err(|e| e.to_string()));
        match spawned {
            Ok(_) => true,
            Err(e) => {
                self.failed(format!("FreeHM was updated but could not restart ({e}); start it again to use the new version"));
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::net::TcpListener;

    /// Serves each `(path, body)` for as many requests as arrive.
    fn serve(files: Vec<(&'static str, Vec<u8>)>) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { return };
                let mut line = String::new();
                let mut r = std::io::BufReader::new(s.try_clone().unwrap());
                r.read_line(&mut line).unwrap();
                let mut h = String::new();
                while r.read_line(&mut h).is_ok_and(|n| n > 2) {
                    h.clear();
                }
                let path = line.split(' ').nth(1).unwrap_or("").to_string();
                match files.iter().find(|(p, _)| *p == path) {
                    Some((_, body)) => {
                        write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                        s.write_all(body).unwrap();
                    }
                    None => write!(s, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap(),
                }
            }
        });
        format!("http://{addr}")
    }

    fn sha(data: &[u8]) -> String {
        Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
    }

    fn asset(base: &str, name: &str) -> Asset {
        Asset { name: name.to_string(), url: format!("{base}/{name}") }
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("freehm-update-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_verified_download_arrives_under_its_final_name() {
        let body = vec![7u8; 300_000];
        let sums = format!("{}  setup.exe\n", sha(&body)).into_bytes();
        let base = serve(vec![("/setup.exe", body.clone()), ("/SHA256SUMS.txt", sums)]);
        let dir = temp("ok");
        let dest = dir.join("setup.exe");
        let mut seen = Vec::new();
        download(&agent(false), &asset(&base, "setup.exe"), &asset(&base, CHECKSUMS_FILE), &dest, |x| seen.push(x)).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert!(!dest.with_extension("part").exists());
        assert!(seen.windows(2).all(|w| w[0] <= w[1]), "{seen:?}");
        assert_eq!(seen.last(), Some(&1.0));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_file_that_does_not_match_its_checksum_is_thrown_away() {
        let sums = format!("{}  setup.exe\n", sha(b"the real file")).into_bytes();
        let base = serve(vec![("/setup.exe", b"something else".to_vec()), ("/SHA256SUMS.txt", sums)]);
        let dir = temp("bad");
        let dest = dir.join("setup.exe");
        let e = download(&agent(false), &asset(&base, "setup.exe"), &asset(&base, CHECKSUMS_FILE), &dest, |_| {}).unwrap_err();
        assert!(e.contains("does not match"), "{e}");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "nothing left behind");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn nothing_is_downloaded_without_a_listed_checksum() {
        let base = serve(vec![("/setup.exe", b"x".to_vec()), ("/SHA256SUMS.txt", format!("{}  other\n", sha(b"x")).into_bytes())]);
        let dir = temp("unlisted");
        let dest = dir.join("setup.exe");
        let e = download(&agent(false), &asset(&base, "setup.exe"), &asset(&base, CHECKSUMS_FILE), &dest, |_| {}).unwrap_err();
        assert!(e.contains("does not list"), "{e}");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);

        let base = serve(vec![("/setup.exe", b"x".to_vec())]);
        let e = download(&agent(false), &asset(&base, "setup.exe"), &asset(&base, CHECKSUMS_FILE), &dest, |_| {}).unwrap_err();
        assert!(e.contains(CHECKSUMS_FILE), "{e}");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn plain_http_is_refused_outside_tests() {
        let base = serve(vec![("/x", b"secret".to_vec())]);
        assert_eq!(fetch_text(&agent(false), &format!("{base}/x")).unwrap(), "secret");
        assert!(fetch_text(&agent(true), &format!("{base}/x")).is_err());
    }

    #[test]
    fn replacing_an_appimage_puts_the_new_file_in_its_place() {
        let dir = temp("appimage");
        let (old, new) = (dir.join("FreeHM.AppImage"), dir.join(APPIMAGE_DOWNLOAD));
        std::fs::write(&old, b"old").unwrap();
        std::fs::write(&new, b"new").unwrap();
        replace_appimage(&new, &old).unwrap();
        assert_eq!(std::fs::read(&old).unwrap(), b"new");
        assert!(!new.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&old).unwrap().permissions().mode() & 0o777, 0o755);
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
