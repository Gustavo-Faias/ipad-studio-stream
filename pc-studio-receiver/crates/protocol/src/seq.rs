/// Conta mensagens perdidas a partir de saltos no `seq` (com volta em 2³²).
///
/// Em `VIDEO_FRAME`, o iPad incrementa o `seq` por cada frame produzido pelo encoder, mesmo que
/// depois o descarte na fila; um salto é, portanto, um "drop frame" do lado do iPad.
#[derive(Debug, Clone, Default)]
pub struct SeqTracker {
    last: Option<u32>,
    received: u64,
    lost: u64,
}

impl SeqTracker {
    /// Regista um `seq` recebido e devolve quantos se perderam desde o anterior.
    pub fn observe(&mut self, seq: u32) -> u32 {
        let gap = match self.last {
            Some(last) => {
                let gap = seq.wrapping_sub(last).wrapping_sub(1);
                // Saltos "para trás" (duplicados, reinício) não contam como perdas.
                if gap < 0x8000_0000 { gap } else { 0 }
            }
            None => 0,
        };
        self.last = Some(seq);
        self.received += 1;
        self.lost += u64::from(gap);
        gap
    }

    pub fn received(&self) -> u64 {
        self.received
    }

    pub fn lost(&self) -> u64 {
        self.lost
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_gaps_across_wraparound() {
        let mut t = SeqTracker::default();
        assert_eq!(t.observe(u32::MAX - 1), 0);
        assert_eq!(t.observe(u32::MAX), 0);
        assert_eq!(t.observe(2), 2); // perderam-se 0 e 1
        assert_eq!(t.observe(2), 0); // duplicado
        assert_eq!((t.received(), t.lost()), (4, 2));
    }
}
