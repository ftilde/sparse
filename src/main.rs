mod devices;
mod log;
mod logout;
mod search;
mod timeline;
mod tui_app;
mod verification_common;
mod verification_initiate;
mod verification_wait;

use matrix_sdk::{self, matrix_auth::MatrixSession, Client};

use structopt::StructOpt;

use std::path::PathBuf;

mod config;
use config::{Config, ConfigBuilder};

const APP_NAME: &str = env!("CARGO_PKG_NAME");
const LOG_RETENTION_POLICY: log::RetentionPolicy = log::RetentionPolicy::Keep(3);

fn try_load_session(config: &Config) -> Result<MatrixSession, Box<dyn std::error::Error>> {
    let session_file = std::fs::File::open(config.session_file_path())?; //TODO: encrypt?
    Ok(serde_json::from_reader(session_file)?)
}

fn try_store_session(
    config: &Config,
    session: &MatrixSession,
) -> Result<(), Box<dyn std::error::Error>> {
    let session_file_path = config.session_file_path();
    std::fs::create_dir_all(session_file_path.parent().unwrap())?;
    let session_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(session_file_path)?;
    serde_json::to_writer(session_file, session)?;
    Ok(())
}

async fn try_restore_session(
    client: &Client,
    config: &Config,
) -> Result<(), Box<dyn std::error::Error>> {
    let session = try_load_session(config)?;
    client.restore_session(session).await?;

    // Test the token which may have been invalidated: We don't actually care about the result, but
    // it will fail if we are not logged in with the old token.
    let _ = client.devices().await?;
    Ok(())
}

async fn login(config: &Config) -> Result<Client, String> {
    // the location for `JsonStore` to save files to
    let data_dir = config.data_dir();

    let client = Client::builder()
        .user_agent(APP_NAME)
        .server_name(&config.host)
        .sqlite_store(data_dir, None);

    // create a new Client with the given homeserver url and config
    let client = match client.build().await {
        Ok(client) => client,
        Err(e) => return Err(format!("{:?}", e)),
    };

    if try_restore_session(&client, &config).await.is_err() {
        eprintln!(
            "Could not restore session. Please provide the password for user {} to log in:",
            config.user
        );

        loop {
            match rpassword::read_password_from_tty(Some("Password: ")) {
                Ok(pw) if pw.is_empty() => {}
                Ok(pw) => {
                    let mut device_name = APP_NAME.to_string();
                    if let Ok(hostname) = hostname::get() {
                        device_name.push_str(&format!(" on {}", hostname.to_string_lossy()));
                    };
                    let login = client
                        .matrix_auth()
                        .login_username(&config.user, &pw)
                        .initial_device_display_name(&device_name);
                    match login.send().await {
                        Ok(response) => {
                            let session = MatrixSession {
                                meta: matrix_sdk::SessionMeta {
                                    user_id: response.user_id,
                                    device_id: response.device_id,
                                },
                                tokens: matrix_sdk::matrix_auth::MatrixSessionTokens {
                                    access_token: response.access_token,
                                    refresh_token: None,
                                },
                            };

                            try_store_session(&config, &session).unwrap();
                            break;
                        }
                        Err(matrix_sdk::Error::Http(matrix_sdk::HttpError::Api(
                            matrix_sdk::ruma::api::error::FromHttpResponseError::Server(
                                matrix_sdk::RumaApiError::ClientApi(r),
                            ),
                        ))) => {
                            eprintln!("{}", r);
                        }
                        Err(e) => {
                            panic!("Unexpected error: {}", e);
                        }
                    }
                }
                Err(e) => panic!("{}", e),
            }
        }
    }
    eprintln!("Logged in as {}", config.user);
    Ok(client)
}

#[derive(StructOpt, Clone)]
struct VerifyInitiate {
    #[structopt()]
    device_id: String,
}

#[derive(StructOpt, Clone)]
struct Logout {
    #[structopt(required = true)]
    device_ids: Vec<String>,
}

#[derive(StructOpt, Clone)]
enum Command {
    #[structopt(about = "Start the interactive tui client (the default action)")]
    Tui,
    #[structopt(about = "List registered devices")]
    Devices,
    #[structopt(about = "Log out (delete) devices from the server")]
    Logout(Logout),
    #[structopt(about = "Start verification of a specific device")]
    VerifyInitiate(VerifyInitiate),
    #[structopt(about = "Wait for incoming device verifications")]
    VerifyWait,
}

#[derive(StructOpt)]
#[structopt(author, about)]
struct Options {
    #[structopt(short = "h", long = "host")]
    host: Option<matrix_sdk::OwnedServerName>,
    #[structopt(short = "u", long = "user")]
    user: Option<String>,
    #[structopt(short = "c", long = "config")]
    config_file: Option<PathBuf>,
    #[structopt(subcommand)]
    command: Option<Command>,
}

impl Options {
    fn command(&self) -> Command {
        self.command.clone().unwrap_or(Command::Tui)
    }
}

fn main() {
    let options = Options::from_args();

    // Perform the init before starting any threads. This is important for setup of signal
    // handling.
    match options.command() {
        Command::Tui => tui_app::init(),
        _ => {}
    }

    // Then start the async runtime and root task
    use tokio::runtime::Runtime;
    let rt = Runtime::new().unwrap();
    if let Err(e) = rt.block_on(tokio_main(options)) {
        eprintln!("{}", e);
    }
}

async fn tokio_main(options: Options) -> Result<(), Box<dyn std::error::Error>> {
    let _log_guard = log::init(LOG_RETENTION_POLICY)?;
    let command = options.command();
    let mut config = ConfigBuilder::new();

    config.configure(include_str!("base_config.lua"))?;

    let config_file = options.config_file.or({
        let f = PathBuf::from(
            dirs::config_dir()
                .unwrap()
                .join(APP_NAME)
                .join("config.lua"),
        );
        if f.exists() {
            Some(f)
        } else {
            None
        }
    });
    if let Some(config_file) = config_file {
        let content = std::fs::read_to_string(config_file)?;
        config.configure(&content)?;
    }

    if let Some(user) = options.user {
        config.set_user(user);
    }
    if let Some(host) = options.host {
        config.set_host(host);
    }

    let (config, key_mapping) = config.finalize()?;

    let client = login(&config).await?;

    match command {
        Command::Tui => tui_app::run(client, config, key_mapping).await?,
        Command::Devices => devices::run(client).await?,
        Command::Logout(l) => logout::run(client, l.device_ids).await?,
        Command::VerifyInitiate(v) => {
            verification_initiate::run(client, v.device_id.clone()).await?
        }
        Command::VerifyWait => verification_wait::run(client).await?,
    }
    Ok(())
}
