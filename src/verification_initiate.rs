use matrix_sdk::encryption::verification::{SasVerification, Verification};
use matrix_sdk::ruma::events::{key::verification::VerificationMethod, AnyToDeviceEvent};

use matrix_sdk::{self, config::SyncSettings, Client, LoopCtrl};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

fn wait_for_confirmation(done: Arc<AtomicBool>) -> Result<(), ()> {
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .expect("error: unable to read user input");

    done.store(true, Ordering::SeqCst);
    match input.trim().to_lowercase().as_ref() {
        "yes" => Ok(()),
        _ => Err(()),
    }
}

pub async fn sleep() {
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}

async fn wait_for_confirmation2(sas: SasVerification) {
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

pub async fn run(client: Client, id: String) -> Result<(), matrix_sdk::Error> {
    eprintln!("Warning: Initiated device verification is still broken.");

    let res = client.sync_once(SyncSettings::new()).await?;
    let user_id = client.user_id().await.unwrap();
    let device = client
        .get_device(&user_id, id.as_str().into())
        .await?
        .unwrap();

    let verification = device
        .request_verification_with_methods(vec![VerificationMethod::SasV1])
        .await?;

    println!("0");
    client
        .sync_with_callback(
            SyncSettings::new()
                .timeout(std::time::Duration::from_secs(1))
                .token(res.next_batch),
            |_| async {
                println!("sync0");
                if verification.is_ready() || verification.is_cancelled() {
                    LoopCtrl::Break
                } else {
                    LoopCtrl::Continue
                }
            },
        )
        .await;
    println!("1");
    if verification.is_cancelled() {
        println!("Verification canceled while waiting for other device to be ready.");
        return Ok(());
    }
    let sas = if let Some(sas) = verification.start_sas().await? {
        sas
    } else {
        println!("Verification canceled (No sas).");
        return Ok(());
    };
    println!("2");
    println!("3");

    let c2 = &client;
    client
        .sync_with_callback(SyncSettings::new(), |response| async move {
            for event in response
                .to_device
                .events
                .iter()
                .filter_map(|e| e.deserialize().ok())
            {
                match event {
                    AnyToDeviceEvent::KeyVerificationStart(e) => {
                        if let Some(Verification::SasV1(sas)) = c2
                            .get_verification(&e.sender, &e.content.transaction_id)
                            .await
                        {
                            println!(
                                "Starting verification with {} {}",
                                &sas.other_device().user_id(),
                                &sas.other_device().device_id()
                            );
                            sas.accept().await.unwrap();
                        }
                    }

                    AnyToDeviceEvent::KeyVerificationKey(e) => {
                        if let Some(Verification::SasV1(sas)) = c2
                            .get_verification(&e.sender, &e.content.transaction_id)
                            .await
                        {
                            tokio::spawn(wait_for_confirmation2(sas));
                        }
                    }

                    AnyToDeviceEvent::KeyVerificationMac(e) => {
                        if let Some(Verification::SasV1(sas)) = c2
                            .get_verification(&e.sender, &e.content.transaction_id)
                            .await
                        {
                            if sas.is_done() {
                                print_result(&sas);
                                return LoopCtrl::Break;
                            }
                        }
                    }
                    _ => (),
                }
            }
            LoopCtrl::Continue
        })
        .await;

    client
        .sync_with_callback(SyncSettings::new(), |_| async {
            if !sas.can_be_presented() || sas.is_cancelled() {
                LoopCtrl::Break
            } else {
                LoopCtrl::Continue
            }
        })
        .await;
    if sas.is_cancelled() {
        println!("Verification canceled while waiting for code.");
        return Ok(());
    }

    println!("Type 'yes' if the emoji or the numbers match:");
    if let Some(emoji) = sas.emoji() {
        print!("Emoji:");
        for e in emoji {
            print!("{} ({}) ", e.symbol, e.description);
        }
        println!();
    } else {
        println!("No emojis available.");
    }
    if let Some((n1, n2, n3)) = sas.decimals() {
        println!("Numbers: {}-{}-{}", n1, n2, n3);
    } else {
        println!("No numbers available.");
    }

    let done = Arc::new(AtomicBool::new(false));
    let done_thread = done.clone();
    let input_task = std::thread::spawn(|| wait_for_confirmation(done_thread));

    while !done.load(Ordering::SeqCst) {
        if sas.is_cancelled() {
            println!("Verification canceled.");
            return Ok(());
        }
        sleep().await;
        let _ = client.sync_once(SyncSettings::new()).await?;
    }

    match input_task.join().unwrap() {
        Ok(()) => {
            sas.confirm().await.unwrap();
        }
        Err(()) => {
            sas.cancel().await.unwrap();
            println!("Interactively canceled verification.");
            return Ok(());
        }
    }

    while !sas.is_done() {
        if sas.is_cancelled() {
            println!("Verification canceled.");
            return Ok(());
        }
        sleep().await;
        let _ = client.sync_once(SyncSettings::new()).await?;
    }
    let device = sas.other_device();

    println!(
        "Successfully verified device {} {} {:?}",
        device.user_id(),
        device.device_id(),
        device.local_trust_state()
    );
    Ok(())
}
