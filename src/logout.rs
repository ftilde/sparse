use matrix_sdk::ruma::api::client::uiaa;
use matrix_sdk::{self, config::SyncSettings, Client};
use std::collections::HashMap;

pub async fn run(client: Client, ids: Vec<String>) -> Result<(), matrix_sdk::Error> {
    let _res = client.sync_once(SyncSettings::new()).await?;

    let response = client.devices().await?;
    let available_ids = response
        .devices
        .iter()
        .map(|d| (d.device_id.as_str(), &d.device_id))
        .collect::<HashMap<_, _>>();

    let mut device_ids = Vec::new();
    for id in &ids {
        if let Some(i) = available_ids.get(id.as_str()) {
            device_ids.push((*i).to_owned());
        } else {
            panic!("'{}' is not the id one of your devices.", id);
        }
    }

    let session = client.session().unwrap();
    let user_id = session.meta().user_id.as_str();

    if let Err(e) = client.delete_devices(&device_ids, None).await {
        if let Some(info) = e.as_uiaa_response() {
            println!("Logging out other devices requires additional password authentication.");
            match rpassword::read_password_from_tty(Some("Password: ")) {
                Ok(pw) if pw.is_empty() => {}
                Ok(pw) => {
                    let mut auth_data = uiaa::Password::new(
                        uiaa::UserIdentifier::UserIdOrLocalpart(user_id.to_string()),
                        pw,
                    );
                    auth_data.session = info.session.clone();
                    let auth_data = uiaa::AuthData::Password(auth_data);
                    client.delete_devices(&device_ids, Some(auth_data)).await?;
                    println!("Done");
                }
                Err(e) => panic!("{}", e),
            }
        }
    } else {
        panic!("Delete succeeded without authentication!");
    }

    Ok(())
}
