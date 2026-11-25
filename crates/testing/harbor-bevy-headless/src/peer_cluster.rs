//! Single-threaded, transport-agnostic multi-peer stepping.

use crate::HeadlessApp;

/// A peer's stable zero-based position in the cluster.
pub type PeerIndex = usize;

type DeliveryHook<A> = Box<dyn FnMut(PeerIndex, &mut A, bool)>;

pub struct PeerCluster<A = HeadlessApp> {
    peers: Vec<A>,
    step: fn(&mut A),
    delivery_hook: Option<DeliveryHook<A>>,
    held: Vec<bool>,
}

impl PeerCluster {
    pub fn new(peers: impl IntoIterator<Item = HeadlessApp>) -> Self {
        Self::with_stepper(peers, |app| {
            app.update();
        })
    }
}

impl<A> PeerCluster<A> {
    /// Supply the single-frame operation for a custom peer type.
    pub fn with_stepper(peers: impl IntoIterator<Item = A>, step: fn(&mut A)) -> Self {
        let peers: Vec<_> = peers.into_iter().collect();
        Self {
            held: vec![false; peers.len()],
            peers,
            step,
            delivery_hook: None,
        }
    }

    /// Install an adapter that pauses/resumes delivery to a peer without
    /// stopping updates. The adapter owns any buffering or FIFO semantics.
    /// Install before use; an installed hook cannot be replaced.
    pub fn with_delivery_hook(
        mut self,
        hook: impl FnMut(PeerIndex, &mut A, bool) + 'static,
    ) -> Self {
        assert!(
            self.delivery_hook.is_none(),
            "delivery hook already installed"
        );
        self.delivery_hook = Some(Box::new(hook));
        self
    }

    pub fn peers(&self) -> &[A] {
        &self.peers
    }

    pub fn peer(&self, index: PeerIndex) -> Option<&A> {
        self.peers.get(index)
    }

    pub fn peer_mut(&mut self, index: PeerIndex) -> Option<&mut A> {
        self.peers.get_mut(index)
    }

    /// Step every peer once in ascending index order.
    pub fn update_all(&mut self) {
        self.peers.iter_mut().for_each(self.step);
    }

    pub fn step_peer(&mut self, index: PeerIndex) -> anyhow::Result<()> {
        self.validate(index)?;
        (self.step)(&mut self.peers[index]);
        Ok(())
    }

    /// Step each requested peer once in ascending index order, ignoring
    /// duplicates. An invalid index leaves every peer untouched.
    pub fn step_subset(&mut self, indices: &[PeerIndex]) -> anyhow::Result<()> {
        for &index in indices {
            self.validate(index)?;
        }
        let mut indices = indices.to_vec();
        indices.sort_unstable();
        indices.dedup();
        for index in indices {
            (self.step)(&mut self.peers[index]);
        }
        Ok(())
    }

    pub fn run_frames_all(&mut self, frames: u64) {
        for _ in 0..frames {
            self.update_all();
        }
    }

    /// Evaluate before stepping and after each full round, up to `max_frames`.
    pub fn run_until(&mut self, max_frames: u64, mut pred: impl FnMut(&Self) -> bool) -> bool {
        for _ in 0..max_frames {
            if pred(self) {
                return true;
            }
            self.update_all();
        }
        pred(self)
    }

    /// Returns whether delivery state changed. Requires a configured hook.
    pub fn hold_delivery(&mut self, index: PeerIndex) -> anyhow::Result<bool> {
        self.set_delivery_held(index, true)
    }

    /// Returns whether delivery state changed. Requires a configured hook.
    pub fn release_delivery(&mut self, index: PeerIndex) -> anyhow::Result<bool> {
        self.set_delivery_held(index, false)
    }

    pub fn delivery_held(&self, index: PeerIndex) -> Option<bool> {
        self.held.get(index).copied()
    }

    fn set_delivery_held(&mut self, index: PeerIndex, held: bool) -> anyhow::Result<bool> {
        self.validate(index)?;
        let hook = self
            .delivery_hook
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("no delivery hook"))?;
        if self.held[index] == held {
            return Ok(false);
        }
        hook(index, &mut self.peers[index], held);
        self.held[index] = held;
        Ok(true)
    }

    fn validate(&self, index: PeerIndex) -> anyhow::Result<()> {
        anyhow::ensure!(index < self.peers.len(), "no peer at index {index}");
        Ok(())
    }
}
