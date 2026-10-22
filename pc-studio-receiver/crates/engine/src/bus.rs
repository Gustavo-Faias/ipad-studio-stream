use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError, bounded};

use crate::MediaEvent;

struct Slot {
    tx: Sender<MediaEvent>,
    /// Eventos perdidos ainda por anunciar com `Gap`.
    gap: u64,
}

/// Distribui eventos de media por várias subscrições, cada uma com a sua fila limitada.
///
/// Quem publica nunca bloqueia. Se a fila de uma subscrição estiver cheia, os eventos dessa
/// subscrição são descartados e ela recebe, no ponto certo do fluxo, um `MediaEvent::Gap(n)`.
/// Os payloads são `Bytes` partilhados: publicar para N subscrições não copia dados.
#[derive(Clone, Default)]
pub struct Bus {
    slots: Arc<Mutex<Vec<Slot>>>,
}

impl Bus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Nova subscrição com fila de `capacity` eventos (ex.: preview 16, gravador 1024).
    pub fn subscribe(&self, capacity: usize) -> Subscription {
        let (tx, rx) = bounded(capacity.max(2));
        self.slots.lock().unwrap().push(Slot { tx, gap: 0 });
        Subscription { rx }
    }

    pub fn publish(&self, event: MediaEvent) {
        let mut slots = self.slots.lock().unwrap();
        slots.retain_mut(|slot| {
            if slot.gap > 0 {
                match slot.tx.try_send(MediaEvent::Gap(slot.gap)) {
                    Ok(()) => slot.gap = 0,
                    Err(TrySendError::Full(_)) => {
                        slot.gap += 1;
                        return true;
                    }
                    Err(TrySendError::Disconnected(_)) => return false,
                }
            }
            match slot.tx.try_send(event.clone()) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => {
                    slot.gap += 1;
                    true
                }
                Err(TrySendError::Disconnected(_)) => false, // subscrição largada
            }
        });
    }

    pub fn subscriber_count(&self) -> usize {
        self.slots.lock().unwrap().len()
    }
}

/// O motor (e com ele o bus) deixou de existir.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusClosed;

impl std::fmt::Display for BusClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("o bus de media foi fechado")
    }
}

impl std::error::Error for BusClosed {}

/// Lado de receção. Largar a subscrição remove-a do bus.
pub struct Subscription {
    rx: Receiver<MediaEvent>,
}

impl Subscription {
    /// Bloqueia até ao próximo evento; `None` se o bus deixou de existir.
    pub fn recv(&self) -> Option<MediaEvent> {
        self.rx.recv().ok()
    }

    /// `Ok(None)` quando o tempo expira; `Err(BusClosed)` se o bus deixou de existir.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Option<MediaEvent>, BusClosed> {
        match self.rx.recv_timeout(timeout) {
            Ok(e) => Ok(Some(e)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(BusClosed),
        }
    }

    pub fn try_recv(&self) -> Option<MediaEvent> {
        self.rx.try_recv().ok()
    }

    /// Para usar com `crossbeam_channel::select!`.
    pub fn receiver(&self) -> &Receiver<MediaEvent> {
        &self.rx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ended(n: u64) -> MediaEvent {
        MediaEvent::SessionEnded { reason: n.to_string() }
    }

    fn label(e: &MediaEvent) -> String {
        match e {
            MediaEvent::SessionEnded { reason } => reason.clone(),
            MediaEvent::Gap(n) => format!("gap{n}"),
            other => format!("{other:?}"),
        }
    }

    #[test]
    fn slow_subscriber_gets_gap_in_order_and_fast_one_gets_everything() {
        let bus = Bus::new();
        let slow = bus.subscribe(2);
        let fast = bus.subscribe(100);
        for i in 0..6 {
            bus.publish(ended(i));
        }
        // O lento só tinha espaço para 2; os 4 seguintes perderam-se.
        assert_eq!(label(&slow.try_recv().unwrap()), "0");
        assert_eq!(label(&slow.try_recv().unwrap()), "1");
        assert!(slow.try_recv().is_none());
        bus.publish(ended(6));
        // O Gap chega antes do evento seguinte, com o número exato de perdidos (2, 3, 4, 5).
        assert_eq!(label(&slow.try_recv().unwrap()), "gap4");
        assert_eq!(label(&slow.try_recv().unwrap()), "6");
        assert_eq!((0..7).map(|_| label(&fast.try_recv().unwrap())).collect::<Vec<_>>(), ["0", "1", "2", "3", "4", "5", "6"]);
    }

    #[test]
    fn dropped_subscription_is_removed() {
        let bus = Bus::new();
        let s = bus.subscribe(4);
        assert_eq!(bus.subscriber_count(), 1);
        drop(s);
        bus.publish(ended(0));
        assert_eq!(bus.subscriber_count(), 0);
    }
}
