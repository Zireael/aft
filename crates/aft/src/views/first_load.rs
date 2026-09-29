//! First load of a checkout: pin a compatible family seed with
//! pin-then-verify, strictly reconcile the checkout against it, serve the seed
//! plus the local delta, then install the view's own generation. Implements
//! `contracts::PlaneLoader`.
