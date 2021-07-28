use matrix_sdk::Client;

pub async fn run(client: Client) -> Result<(), matrix_sdk::Error> {
    let response = client
        .devices()
        .await
        .expect("Can't get devices from server");

    println!("ID\tDevice name",);

    for device in response.devices {
        println!(
            "{}\t{}",
            device.device_id,
            device.display_name.as_deref().unwrap_or("")
        );
    }

    Ok(())
}
