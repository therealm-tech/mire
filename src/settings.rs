//! `~/.config/mire/mire.yaml` — the flags, written down once.
//!
//! Every option `mire` has is a flag, an environment variable, and a key in this
//! file, in that order of precedence: a flag beats the environment, the
//! environment beats the file, the file beats the defaults. The file is for the
//! settings that stopped being a decision — where your profiles live, the CA
//! bundle somebody put on the machine, the port you already bookmarked. A shell
//! alias covers those too, right up until you open a different shell.
//!
//! It is read once, at startup, and never again. The profiles directories are
//! watched because their contents are the input to the tool and change while you
//! work; this file says which directories those are and which address to bind,
//! and neither of those can change under a running process.
//!
//! A broken file is fatal, which is the opposite of the policy for everything
//! *in* the profiles directories. There, coming up and showing the problem beats
//! refusing to start — you reach for `mire` when something is already wrong.
//! Here the file is the tool's own wiring: a `port:` that did not parse means
//! listening somewhere you did not ask for, and a misspelt key means a setting
//! you believe is in effect and is not. Both are worth stopping for.

use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::cli::Cli;

/// Profiles directory used when nothing names one.
pub const DEFAULT_PROFILES: &str = "./profiles";

/// Uploads directory used when nothing names one.
pub const DEFAULT_UPLOADS: &str = "./uploads";

/// Listen address used when nothing names one. Localhost, deliberately.
pub const DEFAULT_HOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// Listen port used when nothing names one.
pub const DEFAULT_PORT: u16 = 8787;

/// `tracing` filter directive used when nothing names one.
pub const DEFAULT_LOG_FILTER: &str = "info";

/// Where the file lives, relative to the configuration home directory.
const FILE: &str = "mire/mire.yaml";

/// Everything `mire` needs to start, with every source already folded in.
#[derive(Debug)]
pub struct Settings {
    /// Directories holding the profile YAML files, in precedence order.
    pub profiles: Vec<PathBuf>,
    /// Directory attached files are written to.
    pub uploads: PathBuf,
    /// Address to listen on.
    pub host: IpAddr,
    /// Port to listen on.
    pub port: u16,
    /// Path prefix for every route.
    pub base_path: String,
    /// Origin the browser reaches `mire` on, for the OIDC callback.
    pub public_url: Option<String>,
    /// PEM bundle of extra certificate authorities to trust.
    pub ca_bundle: Option<PathBuf>,
    /// `tracing` filter directive.
    pub log_filter: String,
    /// What became of the file, so startup can say which one it read.
    pub file: FileState,
}

/// What became of the configuration file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileState {
    /// Read, and its keys are in these settings.
    Read(PathBuf),
    /// Looked for here and not there — the ordinary case for somebody who never
    /// wrote one, and therefore not a complaint.
    Absent(PathBuf),
    /// Nowhere to look: no `--config`, and no home directory to find one in.
    Nowhere,
}

impl Settings {
    /// Resolves every option: flags and environment first, then the file, then
    /// the defaults.
    ///
    /// Deliberately silent — this runs before the `tracing` subscriber exists,
    /// because the file may carry the filter that configures it, so anything it
    /// logged would go nowhere. [`Settings::file`] is what startup reports
    /// afterwards.
    ///
    /// # Errors
    ///
    /// Fails if the file cannot be read or does not parse. A file named with
    /// `--config` that is not there is an error; the default location not
    /// existing is not.
    pub fn resolve(cli: &Cli) -> Result<Self, SettingsError> {
        Self::resolve_in(cli, &Home::from_env())
    }

    /// Same, against a given home directory.
    ///
    /// The seam the tests use: `std::env::set_var` is `unsafe` in this edition
    /// and the crate forbids `unsafe`, so `HOME` is a parameter rather than
    /// something a test can arrange.
    fn resolve_in(cli: &Cli, home: &Home) -> Result<Self, SettingsError> {
        let (file, state) = SettingsFile::find(cli.config.as_deref(), home)?;

        Ok(Self {
            profiles: cli
                .profiles
                .clone()
                .or_else(|| file.profiles.map(|paths| home.expand_all(paths.into_vec())))
                .unwrap_or_else(|| vec![PathBuf::from(DEFAULT_PROFILES)]),
            uploads: cli
                .uploads
                .clone()
                .or_else(|| file.uploads.map(|path| home.expand(path)))
                .unwrap_or_else(|| PathBuf::from(DEFAULT_UPLOADS)),
            host: cli.host.or(file.host).unwrap_or(DEFAULT_HOST),
            port: cli.port.or(file.port).unwrap_or(DEFAULT_PORT),
            base_path: cli.base_path.clone().or(file.base_path).unwrap_or_default(),
            public_url: cli.public_url.clone().or(file.public_url),
            ca_bundle: cli
                .ca_bundle
                .clone()
                .or_else(|| file.ca_bundle.map(|path| home.expand(path))),
            log_filter: cli
                .log_filter
                .clone()
                .or(file.log_filter)
                .unwrap_or_else(|| DEFAULT_LOG_FILTER.to_owned()),
            file: state,
        })
    }
}

/// Why `mire` could not make sense of the configuration file.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    // The path is in the message: the file may have been found by convention
    // rather than named, and then "no such file or directory" does not say which
    // file anybody was talking about.
    #[error("cannot read the configuration file {path}: {source}")]
    Read {
        /// File that could not be read.
        path: PathBuf,
        /// What the filesystem said.
        source: std::io::Error,
    },

    #[error("cannot parse the configuration file {path}: {source}")]
    Parse {
        /// File that did not parse.
        path: PathBuf,
        /// What the parser said, position included.
        source: serde_yaml_ng::Error,
    },
}

/// The document itself: every key optional, because a file that sets one thing is
/// the normal way to use it.
///
/// `deny_unknown_fields` on purpose. The alternative is a `log_fitler:` that
/// parses cleanly and does nothing, which is the worst answer available.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsFile {
    profiles: Option<Directories>,
    uploads: Option<PathBuf>,
    host: Option<IpAddr>,
    port: Option<u16>,
    base_path: Option<String>,
    public_url: Option<String>,
    ca_bundle: Option<PathBuf>,
    log_filter: Option<String>,
}

impl SettingsFile {
    /// Reads the file `named` on the command line, or the one at the default
    /// location, or neither.
    fn find(named: Option<&Path>, home: &Home) -> Result<(Self, FileState), SettingsError> {
        let Some(path) = named.map(Path::to_path_buf).or_else(|| home.config_file()) else {
            return Ok((Self::default(), FileState::Nowhere));
        };

        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let file =
                    serde_yaml_ng::from_str(&text).map_err(|source| SettingsError::Parse {
                        path: path.clone(),
                        source,
                    })?;
                Ok((file, FileState::Read(path)))
            }
            // Nobody has to write this file. Somebody who *named* one does have
            // to spell it right, though — a typo there must not read as "you have
            // no configuration file", which is exactly what would happen if the
            // defaults quietly took over.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && named.is_none() => {
                Ok((Self::default(), FileState::Absent(path)))
            }
            Err(source) => Err(SettingsError::Read { path, source }),
        }
    }
}

/// One directory, or a list of them.
///
/// `profiles: /etc/mire/profiles` and `profiles: [a, b]` both say something
/// obvious, and rejecting the first because it is not a sequence would be
/// pedantry. The flag's `:`-separated spelling is not repeated here: a file that
/// can hold a list has no reason to pack one into a string.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Directories {
    One(PathBuf),
    Many(Vec<PathBuf>),
}

impl Directories {
    fn into_vec(self) -> Vec<PathBuf> {
        match self {
            Self::One(path) => vec![path],
            Self::Many(paths) => paths,
        }
    }
}

/// The two environment variables that decide where a home-relative path points.
#[derive(Debug, Default)]
struct Home {
    /// `$HOME`, for expanding a leading `~`.
    home: Option<PathBuf>,
    /// `$XDG_CONFIG_HOME`, or `$HOME/.config` — where the file lives by default.
    config: Option<PathBuf>,
}

impl Home {
    fn from_env() -> Self {
        let home = std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|home| home.join(".config")));
        Self { home, config }
    }

    /// Default path of the configuration file, when there is a home to put it in.
    fn config_file(&self) -> Option<PathBuf> {
        self.config.as_ref().map(|dir| dir.join(FILE))
    }

    /// Expands a leading `~` in a path the file gave us.
    ///
    /// Only the file's paths get this. A flag or an environment assignment is
    /// written in a shell, which expands `~` before `mire` sees anything; a YAML
    /// document has no shell behind it, and a file that lives in the home
    /// directory is precisely where somebody writes `~/profiles`.
    ///
    /// `~someone-else` is left alone: resolving another user's home needs the
    /// password database, and a wrong guess is worse than the literal path.
    fn expand(&self, path: PathBuf) -> PathBuf {
        match (self.home.as_deref(), path.strip_prefix("~")) {
            (Some(home), Ok(rest)) => home.join(rest),
            _ => path,
        }
    }

    fn expand_all(&self, paths: Vec<PathBuf>) -> Vec<PathBuf> {
        paths.into_iter().map(|path| self.expand(path)).collect()
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use tempfile::TempDir;

    use super::*;

    /// A home directory with `mire.yaml` in it, or without.
    struct Fixture {
        dir: TempDir,
        home: Home,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = TempDir::new().expect("temp dir");
            let home = Home {
                home: Some(dir.path().to_path_buf()),
                config: Some(dir.path().join(".config")),
            };
            Self { dir, home }
        }

        fn write(&self, body: &str) -> &Self {
            let path = self.home.config_file().expect("a configured home");
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("create dir");
            std::fs::write(path, body).expect("write file");
            self
        }

        fn resolve(&self, args: &[&str]) -> Result<Settings, SettingsError> {
            let cli = Cli::try_parse_from(std::iter::once("mire").chain(args.iter().copied()))
                .expect("parse");
            Settings::resolve_in(&cli, &self.home)
        }

        fn settings(&self, args: &[&str]) -> Settings {
            self.resolve(args).expect("resolve")
        }
    }

    #[test]
    fn nothing_anywhere_is_every_default() {
        let settings = Fixture::new().settings(&[]);

        assert_eq!(settings.profiles, [PathBuf::from(DEFAULT_PROFILES)]);
        assert_eq!(settings.uploads, PathBuf::from(DEFAULT_UPLOADS));
        assert_eq!(settings.host, DEFAULT_HOST);
        assert_eq!(settings.port, DEFAULT_PORT);
        assert_eq!(settings.base_path, "");
        assert_eq!(settings.public_url, None);
        assert_eq!(settings.ca_bundle, None);
        assert_eq!(settings.log_filter, DEFAULT_LOG_FILTER);
    }

    #[test]
    fn the_file_supplies_every_option() {
        let fixture = Fixture::new();
        fixture.write(
            "profiles:\n  - /etc/mire/profiles\n  - /srv/mine\nuploads: /var/lib/mire/uploads\nhost: 0.0.0.0\nport: 9000\nbase_path: /proxy/8787\npublic_url: https://kubeflow.example\nca_bundle: /etc/ssl/internal.pem\nlog_filter: mire=debug\n",
        );

        let settings = fixture.settings(&[]);

        assert_eq!(
            settings.profiles,
            [
                PathBuf::from("/etc/mire/profiles"),
                PathBuf::from("/srv/mine")
            ]
        );
        assert_eq!(settings.uploads, PathBuf::from("/var/lib/mire/uploads"));
        assert_eq!(settings.host, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        assert_eq!(settings.port, 9000);
        assert_eq!(settings.base_path, "/proxy/8787");
        assert_eq!(
            settings.public_url.as_deref(),
            Some("https://kubeflow.example")
        );
        assert_eq!(
            settings.ca_bundle,
            Some(PathBuf::from("/etc/ssl/internal.pem"))
        );
        assert_eq!(settings.log_filter, "mire=debug");
        assert!(matches!(settings.file, FileState::Read(_)));
    }

    /// The whole point of the order: the file is what you always want, the flag
    /// is what you want this afternoon.
    #[test]
    fn a_flag_beats_the_file() {
        let fixture = Fixture::new();
        fixture.write("profiles:\n  - /etc/mire/profiles\nport: 9000\n");

        let settings = fixture.settings(&["--profiles", "./mine", "--port", "1234"]);

        assert_eq!(settings.profiles, [PathBuf::from("./mine")]);
        assert_eq!(settings.port, 1234);
    }

    /// A flag that names one directory replaces the file's list rather than
    /// layering on it — the same rule the flag has always had against its own
    /// default.
    #[test]
    fn a_flag_replaces_the_files_list_rather_than_extending_it() {
        let fixture = Fixture::new();
        fixture.write("profiles:\n  - /etc/mire/profiles\n  - /srv/mine\n");

        let settings = fixture.settings(&["--profiles", "./mine"]);

        assert_eq!(settings.profiles, [PathBuf::from("./mine")]);
    }

    #[test]
    fn one_profiles_directory_may_be_written_as_a_string() {
        let fixture = Fixture::new();
        fixture.write("profiles: /etc/mire/profiles\n");

        assert_eq!(
            fixture.settings(&[]).profiles,
            [PathBuf::from("/etc/mire/profiles")]
        );
    }

    #[test]
    fn a_leading_tilde_is_the_home_directory() {
        let fixture = Fixture::new();
        fixture.write("profiles:\n  - ~/profiles\nca_bundle: ~/certs/internal.pem\n");

        let settings = fixture.settings(&[]);

        assert_eq!(settings.profiles, [fixture.dir.path().join("profiles")]);
        assert_eq!(
            settings.ca_bundle,
            Some(fixture.dir.path().join("certs/internal.pem"))
        );
    }

    /// `~someone-else` needs the password database to resolve. The literal path
    /// is a better answer than a guess.
    #[test]
    fn another_users_tilde_is_left_alone() {
        let fixture = Fixture::new();
        fixture.write("profiles:\n  - ~someone/profiles\n");

        assert_eq!(
            fixture.settings(&[]).profiles,
            [PathBuf::from("~someone/profiles")]
        );
    }

    #[test]
    fn a_misspelt_key_is_an_error_that_names_it() {
        let fixture = Fixture::new();
        fixture.write("log_fitler: mire=debug\n");

        let error = fixture.resolve(&[]).expect_err("a typo is fatal");

        assert!(matches!(error, SettingsError::Parse { .. }), "{error:?}");
        assert!(error.to_string().contains("log_fitler"), "{error}");
    }

    #[test]
    fn a_syntax_error_is_an_error_that_says_where() {
        let fixture = Fixture::new();
        fixture.write("profiles: [unclosed\n");

        let error = fixture.resolve(&[]).expect_err("broken YAML is fatal");

        assert!(error.to_string().contains("line"), "{error}");
    }

    /// Most people never write this file, and that is not a failure.
    #[test]
    fn no_file_at_the_default_location_is_not_a_problem() {
        let fixture = Fixture::new();

        let settings = fixture.settings(&[]);

        assert_eq!(
            settings.file,
            FileState::Absent(fixture.home.config_file().expect("a configured home"))
        );
    }

    /// Naming a file is a promise that it exists. Falling back to the defaults
    /// would turn a typo into a silently different configuration.
    #[test]
    fn a_file_named_on_the_command_line_has_to_be_there() {
        let fixture = Fixture::new();
        let missing = fixture.dir.path().join("nowhere.yaml");

        let error = fixture
            .resolve(&["--config", &missing.display().to_string()])
            .expect_err("a named file that is missing is fatal");

        assert!(matches!(error, SettingsError::Read { .. }), "{error:?}");
        assert!(error.to_string().contains("nowhere.yaml"), "{error}");
    }

    #[test]
    fn a_named_file_is_read_instead_of_the_default_one() {
        let fixture = Fixture::new();
        fixture.write("port: 9000\n");
        let other = fixture.dir.path().join("other.yaml");
        std::fs::write(&other, "port: 1234\n").expect("write file");

        let settings = fixture.settings(&["--config", &other.display().to_string()]);

        assert_eq!(settings.port, 1234);
        assert_eq!(settings.file, FileState::Read(other));
    }

    #[test]
    fn the_default_location_is_under_the_configuration_home() {
        let home = Home {
            home: Some(PathBuf::from("/home/you")),
            config: Some(PathBuf::from("/home/you/.config")),
        };

        assert_eq!(
            home.config_file(),
            Some(PathBuf::from("/home/you/.config/mire/mire.yaml"))
        );
    }

    /// A container with no `HOME` still has to come up: there is simply no file
    /// to look for, and the flags and the environment are all of it.
    #[test]
    fn no_home_at_all_still_starts() {
        let cli = Cli::try_parse_from(["mire"]).expect("parse");

        let settings = Settings::resolve_in(&cli, &Home::default()).expect("resolve");

        assert_eq!(settings.file, FileState::Nowhere);
        assert_eq!(settings.port, DEFAULT_PORT);
    }
}
