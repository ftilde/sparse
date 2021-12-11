use matrix_sdk::encryption::verification::{SasVerification, Verification};
use matrix_sdk::ruma::events::{key::verification::VerificationMethod, AnyToDeviceEvent};

use matrix_sdk::{self, config::SyncSettings, Client, LoopCtrl};
use std::sync::atomic::{AtomicBool, Ordering};

async fn wait_for_confirmation(sas: SasVerification) {
    println!("Type 'yes' if the emoji or the numbers match:");
    if let Some(emoji) = sas.emoji() {
        print!("Emoji:");
        for e in emoji {
            print!("{} ({}) ", e.symbol, e.description);
        }
        println!();
    }
    if let Some((n1, n2, n3)) = sas.decimals() {
        println!("Numbers: {}-{}-{}", n1, n2, n3);
    }

    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .expect("error: unable to read user input");

    match input.trim().to_lowercase().as_ref() {
        "yes" => {
            sas.confirm().await.unwrap();

            if sas.is_done() {
                print_result(&sas);
            }
        }
        _ => {
            println!("Canceled verification.");
            sas.cancel().await.unwrap();
        }
    }
}

fn print_result(sas: &SasVerification) {
    let device = sas.other_device();

    println!(
        "Successfully verified device {} {} {:?}",
        device.user_id(),
        device.device_id(),
        device.local_trust_state()
    );
}

pub async fn run_verify_loop(client: &Client) -> Result<(), matrix_sdk::Error> {
    let user_id = client.user_id().await.unwrap();
    let device_id = client.device_id().await.unwrap();
    let user_id = &user_id;
    let device_id = &device_id;
    let we_started = AtomicBool::new(false);
    let we_started = &we_started;
    client
        .sync_with_callback(SyncSettings::new(), |response| async move {
            for event in response
                .to_device
                .events
                .iter()
                .filter_map(|e| e.deserialize().ok())
            {
                match event {
                    AnyToDeviceEvent::KeyVerificationRequest(e) => {
                        println!("== Request {:?}", e);
                        let request = client
                            .get_verification_request(&e.sender, &e.content.transaction_id)
                            .await;
                        //println!("Got sas {:?}", request);
                        if let Some(request) = request {
                            request
                                .accept_with_methods(vec![VerificationMethod::SasV1])
                                .await
                                .unwrap();

                            let _device = client
                                .get_device(&e.sender, &e.content.from_device)
                                .await
                                .unwrap();
                            //println!("device {:?}", device);
                            let _sas = request.start_sas().await.unwrap();
                            we_started.store(true, Ordering::SeqCst);
                            //println!("sas {:?}", sas);
                        }
                    }
                    AnyToDeviceEvent::KeyVerificationReady(e) => {
                        println!("== Ready {:?}", e);
                        let request = client
                            .get_verification_request(&e.sender, &e.content.transaction_id)
                            .await;
                        println!("Got sas {:?}", request);
                        if let Some(request) = request {
                            //let device = client
                            //    .get_device(&e.sender, &e.content.from_device)
                            //    .await
                            //    .unwrap();
                            //println!("device {:?}", device);
                            let sas = request.start_sas().await.unwrap();
                            we_started.store(true, Ordering::SeqCst);
                            println!("sas {:?}", sas);
                        }
                    }

                    AnyToDeviceEvent::KeyVerificationStart(e) => {
                        println!("== Start: {:?}", e);
                        if let Some(Verification::SasV1(sas)) = client
                            .get_verification(&e.sender, &e.content.transaction_id)
                            .await
                        {
                            let accept = if we_started.load(Ordering::SeqCst) {
                                use std::cmp::Ordering;
                                println!("blub");
                                match (
                                    sas.other_user_id().cmp(user_id),
                                    sas.other_device().device_id().cmp(device_id),
                                ) {
                                    //(Ordering::Less, _) | (Ordering::Equal, Ordering::Less) => {
                                    (Ordering::Less, _) | (Ordering::Equal, Ordering::Greater) => {
                                        //TODO: change back to less. This is only for debuggping purposes
                                        println!(
                                            "Letting other user/device begin verification process"
                                        );
                                        false
                                    }
                                    _ => true,
                                }
                            } else {
                                true
                            };
                            if accept {
                                println!(
                                    "Starting verification with {} {}",
                                    &sas.other_device().user_id(),
                                    &sas.other_device().device_id()
                                );
                                sas.accept().await.unwrap();
                            }
                        }
                    }

                    AnyToDeviceEvent::KeyVerificationKey(e) => {
                        println!("== Key: {:?}", e);
                        if let Some(Verification::SasV1(sas)) = client
                            .get_verification(&e.sender, &e.content.transaction_id)
                            .await
                        {
                            tokio::spawn(wait_for_confirmation(sas));
                        }
                    }

                    AnyToDeviceEvent::KeyVerificationMac(e) => {
                        println!("== Mac: {:?}", e);
                        if let Some(Verification::SasV1(sas)) = client
                            .get_verification(&e.sender, &e.content.transaction_id)
                            .await
                        {
                            if sas.is_done() {
                                print_result(&sas);
                                return LoopCtrl::Break;
                            }
                        }
                    }

                    AnyToDeviceEvent::KeyVerificationAccept(e) => {
                        println!("== Accept: {:?}", e);
                        if let Some(Verification::SasV1(sas)) = client
                            .get_verification(&e.sender, &e.content.transaction_id)
                            .await
                        {
                            if sas.is_done() {
                                print_result(&sas);
                                return LoopCtrl::Break;
                            }
                        }
                    }
                    AnyToDeviceEvent::KeyVerificationCancel(e) => {
                        println!("Verification has been canceled {}", e.content.reason);
                        return LoopCtrl::Break;
                    }
                    o => {
                        println!("other event {:?}", o);
                    }
                }
            }
            LoopCtrl::Continue
        })
        .await;
    Ok(())
}
