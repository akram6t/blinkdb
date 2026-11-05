use std::sync::Arc;
use bytes::Bytes;
use crossbeam_channel::{bounded, Receiver, Sender};

/// Message sent between worker shards for cross-core operations.
pub enum ShardMessage {
    Get {
        key: Bytes,
        resp_tx: oneshot::Sender<Option<Bytes>>,
    },
    Set {
        key: Bytes,
        value: Bytes,
        ttl_ms: Option<u64>,
        resp_tx: oneshot::Sender<bool>,
    },
    Del {
        key: Bytes,
        resp_tx: oneshot::Sender<bool>,
    },
    KeyCount {
        resp_tx: oneshot::Sender<usize>,
    },
}

/// Simple oneshot channel for thread-per-core cross-shard responses.
pub mod oneshot {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use parking_lot::{Condvar, Mutex};

    pub(crate) struct Shared<T> {
        pub(crate) value: Mutex<Option<T>>,
        pub(crate) ready: AtomicBool,
        pub(crate) cvar: Condvar,
    }

    pub struct Sender<T> {
        shared: Arc<Shared<T>>,
    }

    pub struct Receiver<T> {
        shared: Arc<Shared<T>>,
    }

    pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
        let shared = Arc::new(Shared {
            value: Mutex::new(None),
            ready: AtomicBool::new(false),
            cvar: Condvar::new(),
        });
        (
            Sender { shared: shared.clone() },
            Receiver { shared },
        )
    }

    impl<T> Sender<T> {
        pub fn send(self, val: T) {
            *self.shared.value.lock() = Some(val);
            self.shared.ready.store(true, Ordering::Release);
            self.shared.cvar.notify_one();
        }
    }

    impl<T> Receiver<T> {
        pub fn recv(self) -> Option<T> {
            let mut lock = self.shared.value.lock();
            while !self.shared.ready.load(Ordering::Acquire) {
                self.shared.cvar.wait(&mut lock);
            }
            lock.take()
        }

        pub fn try_recv(&self) -> Option<T> {
            if self.shared.ready.load(Ordering::Acquire) {
                self.shared.value.lock().take()
            } else {
                None
            }
        }
    }
}

/// Cross-shard communication hub.
#[derive(Clone)]
pub struct ShardChannelHub {
    senders: Arc<Vec<Sender<ShardMessage>>>,
}

impl ShardChannelHub {
    pub fn new(num_shards: usize) -> (Self, Vec<Receiver<ShardMessage>>) {
        let mut senders = Vec::with_capacity(num_shards);
        let mut receivers = Vec::with_capacity(num_shards);

        for _ in 0..num_shards {
            let (tx, rx) = bounded(16384);
            senders.push(tx);
            receivers.push(rx);
        }

        (
            Self {
                senders: Arc::new(senders),
            },
            receivers,
        )
    }

    #[inline]
    pub fn send_to_shard(&self, target_shard: usize, msg: ShardMessage) -> Result<(), String> {
        if target_shard < self.senders.len() {
            self.senders[target_shard]
                .try_send(msg)
                .map_err(|e| format!("Shard {} queue full or closed: {}", target_shard, e))
        } else {
            Err(format!("Invalid target shard id {}", target_shard))
        }
    }

    pub fn num_shards(&self) -> usize {
        self.senders.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cross_shard_messaging() {
        let (hub, receivers) = ShardChannelHub::new(2);
        let rx0 = &receivers[0];

        let (tx, rx) = oneshot::channel();
        hub.send_to_shard(
            0,
            ShardMessage::Get {
                key: Bytes::from("hello"),
                resp_tx: tx,
            },
        )
        .unwrap();

        if let Ok(ShardMessage::Get { key, resp_tx }) = rx0.try_recv() {
            assert_eq!(key, "hello");
            resp_tx.send(Some(Bytes::from("world")));
        } else {
            panic!("Expected message on shard 0");
        }

        assert_eq!(rx.recv(), Some(Some(Bytes::from("world"))));
    }
}
