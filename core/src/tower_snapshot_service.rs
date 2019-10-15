use crate::consensus::Tower;
use crate::result::Error;
use crate::result::Result;
use crate::service::Service;
use solana_sdk::clock::{DEFAULT_TICKS_PER_SECOND, DEFAULT_TICKS_PER_SLOT};
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::{Builder, JoinHandle};
use std::time::Duration;
use std::{fs, thread};

static TOWER_SNAPSHOT_NAME: &'static str = "tower";

pub type TowerReceiver = Receiver<Tower>;
pub type TowerSender = Sender<Tower>;

fn snapshot(snapshot_path: &PathBuf, tower_receiver: &TowerReceiver) -> Result<()> {
    // should try and snapshot the tower every slot
    const MILLIS_PER_SLOT: u64 = 1000 * DEFAULT_TICKS_PER_SLOT / DEFAULT_TICKS_PER_SECOND;
    let mut tower = tower_receiver.recv_timeout(Duration::from_millis(MILLIS_PER_SLOT))?;
    // only the latest needs to be stored
    while let Ok(new_tower) = tower_receiver.try_recv() {
        tower = new_tower;
    }

    fs::create_dir_all(snapshot_path)?;
    let mut snapshot_file = File::create(snapshot_path.join(TOWER_SNAPSHOT_NAME))
        .expect("unable to create file for tower snapshot");
    snapshot_file.write_all(&bincode::serialize(&tower).expect("tower serialize failed"))?;
    snapshot_file.flush()?;
    Ok(())
}

pub struct TowerSnapshotService {
    t_snapshot: JoinHandle<()>,
}

impl TowerSnapshotService {
    pub fn new(
        snapshot_path: PathBuf,
        tower_receiver: TowerReceiver,
        exit: &Arc<AtomicBool>,
    ) -> Self {
        let exit = exit.clone();
        let t_snapshot = Builder::new()
            .name("solana-tower-snapshot-service".to_string())
            .spawn(move || loop {
                if exit.load(Ordering::Relaxed) {
                    break;
                }
                if let Err(e) = snapshot(&snapshot_path, &tower_receiver) {
                    match e {
                        Error::RecvTimeoutError(RecvTimeoutError::Disconnected) => break,
                        Error::RecvTimeoutError(RecvTimeoutError::Timeout) => (),
                        _ => info!("Error from tower snapshot service: {:?}", e),
                    }
                }
            })
            .unwrap();
        Self { t_snapshot }
    }
}

impl Service for TowerSnapshotService {
    type JoinReturnType = ();

    fn join(self) -> thread::Result<()> {
        self.t_snapshot.join()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_sdk::signature::{Keypair, KeypairUtil};
    use std::io::BufReader;
    use std::sync::mpsc::channel;
    use std::thread::sleep;

    fn tmp_file_path(name: &str) -> String {
        use std::env;
        let out_dir = env::var("FARF_DIR").unwrap_or_else(|_| "farf".to_string());
        let keypair = Keypair::new();

        format!("{}/tmp/{}-{}", out_dir, name, keypair.pubkey()).to_string()
    }

    #[test]
    fn test_tower_snapshot_service_exit() {
        let (_tower_sender, tower_receiver) = channel();
        let exit = Arc::new(AtomicBool::new(false));
        let svc =
            TowerSnapshotService::new(PathBuf::from(tmp_file_path("tower")), tower_receiver, &exit);
        exit.store(true, Ordering::Relaxed);
        svc.join().unwrap();
    }

    #[test]
    fn test_tower_snapshot() {
        let (tower_sender, tower_receiver) = channel();
        let exit = Arc::new(AtomicBool::new(false));
        let snapshot_path = PathBuf::from(tmp_file_path("tower"));
        let svc = TowerSnapshotService::new(snapshot_path.clone(), tower_receiver, &exit);
        tower_sender.send(Tower::default()).unwrap();

        // let it perform the snapshot
        sleep(Duration::from_secs(1));

        // make sure the tower file is present
        let tower_file = File::open(snapshot_path.join(TOWER_SNAPSHOT_NAME)).unwrap();
        let mut stream = BufReader::new(tower_file);
        // make sure it deserializes
        let _tower: Tower = bincode::deserialize_from(&mut stream).unwrap();

        exit.store(true, Ordering::Relaxed);
        svc.join().unwrap();
    }
}
