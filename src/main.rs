mod devices;
mod timeline;
mod tui;
mod tui_app;
mod verification_initiate;
mod verification_wait;

use matrix_sdk::{self, Client, ClientConfig, Session};

use structopt::StructOpt;
use url::Url;

use std::path::PathBuf;

impl Config {
    fn user_id(&self) -> String {
        format!("@{}:{}", self.user, self.host.host_str().unwrap_or(""))
    }

    fn data_dir(&self) -> PathBuf {
        dirs::data_local_dir()
            .unwrap()
            .join(APP_NAME)
            .join(self.user_id())
    }

    fn session_file_path(&self) -> PathBuf {
        self.data_dir().join("session")
    }
}

fn try_load_session(config: &Config) -> Result<Session, Box<dyn std::error::Error>> {
    let session_file = std::fs::File::open(config.session_file_path())?; //TODO: encrypt?
    Ok(serde_json::from_reader(session_file)?)
}

fn try_store_session(config: &Config, session: &Session) -> Result<(), Box<dyn std::error::Error>> {
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

const APP_NAME: &str = env!("CARGO_PKG_NAME");

async fn try_restore_session(
    client: &Client,
    config: &Config,
) -> Result<(), Box<dyn std::error::Error>> {
    let session = try_load_session(config)?;
    client.restore_login(session).await?;

    // Test the token which may have been invalidated: We don't actually care about the result, but
    // it will fail if we are not logged in with the old token.
    let _ = client.devices().await?;
    Ok(())
}

async fn login(config: Config) -> Result<Client, matrix_sdk::Error> {
    // the location for `JsonStore` to save files to
    let data_dir = config.data_dir();
    let client_config = ClientConfig::new().store_path(data_dir);
    // create a new Client with the given homeserver url and config
    let client = Client::new_with_config(config.host.clone(), client_config).unwrap();

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
                    let response = client
                        .login(&config.user, &pw, None, Some(&device_name))
                        .await;
                    match response {
                        Ok(response) => {
                            let session = Session {
                                access_token: response.access_token,
                                user_id: response.user_id,
                                device_id: response.device_id,
                            };

                            try_store_session(&config, &session).unwrap();
                            break;
                        }
                        Err(matrix_sdk::Error::Http(matrix_sdk::HttpError::ClientApi(
                            matrix_sdk::ruma::api::error::FromHttpResponseError::Http(
                                matrix_sdk::ruma::api::error::ServerError::Known(r),
                            ),
                        ))) => {
                            eprintln!("{}", r.message);
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

#[derive(StructOpt)]
struct Config {
    #[structopt(short = "h", long = "host")]
    host: Url,
    #[structopt(short = "u", long = "user")]
    user: String,
}

#[derive(StructOpt)]
struct VerifyInitiate {
    #[structopt()]
    device_id: String,
}

#[derive(StructOpt)]
enum Command {
    #[structopt(about = "Start the interactive tui client (the default action)")]
    Tui,
    #[structopt(about = "List registered devices")]
    Devices,
    #[structopt(about = "Start verification of a specific device")]
    VerifyInitiate(VerifyInitiate),
    #[structopt(about = "Wait for incoming device verifications")]
    VerifyWait,
}

#[derive(StructOpt)]
#[structopt(author, about)]
struct Options {
    #[structopt(flatten)]
    config: Config,
    #[structopt(subcommand)]
    command: Option<Command>,
}

#[tokio::main]
async fn main() -> Result<(), matrix_sdk::Error> {
    //TODO: remove dirty dirty dirty hack with leak here
    let file = &*Box::leak(Box::new(std::fs::File::create("heyo.log").unwrap()));
    tracing_subscriber::fmt()
        .with_writer(move || file)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let options = Options::from_args();

    let client = login(options.config).await?;

    match options.command.unwrap_or(Command::Tui) {
        Command::Tui => tui_app::run(client).await,
        Command::Devices => devices::run(client).await,
        Command::VerifyInitiate(v) => verification_initiate::run(client, v.device_id).await,
        Command::VerifyWait => verification_wait::run(client).await,
    }
}
