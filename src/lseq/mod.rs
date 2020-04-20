mod lseq;
mod nodes;

use crate::traits::{Causal, CmRDT};
use crate::vclock::{Actor, VClock};
pub use lseq::{LSeq, Op};
use std::fmt::Display;

impl<V: Ord + Clone + PartialEq + Display, A: Actor + Display> PartialEq for LSeq<V, A> {
    fn eq(&self, other: &Self) -> bool {
        for (_, (dot, _)) in &self.siblings {
            let num_found = other.siblings.iter().filter(|(_, (d, _))| d == dot).count();

            if num_found == 0 {
                return false;
            }
            // sanity check
            assert_eq!(num_found, 1);
        }
        for (_, (dot, _)) in &other.siblings {
            let num_found = self.siblings.iter().filter(|(_, (d, _))| d == dot).count();

            if num_found == 0 {
                return false;
            }
            // sanity check
            assert_eq!(num_found, 1);
        }
        true
    }
}

impl<V: Ord + Clone + Eq + Display, A: Actor + Display> Eq for LSeq<V, A> {}

impl<V: Ord + Clone + Clone + Display, A: Actor + Display> Causal<A> for LSeq<V, A> {
    fn forget(&mut self, clock: &VClock<A>) {
        self.forget_clock(clock);
    }
}

impl<V: Ord + Clone + Display, A: Actor + Display> CmRDT for LSeq<V, A> {
    type Op = Op<V, A>;

    fn apply(&mut self, op: Self::Op) {
        match op {
            Op::Insert { clock, value, p, q } => {
                if clock.is_empty() {
                    return;
                }

                println!("\n\nINSERTING {} between {:?} and {:?}", value, p, q);

                // Allocate a new identifier between on p and q
                self.alloc_id(p, q, clock, value);
            }
            Op::Delete { id, .. } => {
                println!("\n\nDELETING {}", id);
                // Delete value from the atom which corresponds to the given identifier
                self.delete_id(id);
            }
        }
    }
}
