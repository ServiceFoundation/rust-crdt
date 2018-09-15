use serde::de::DeserializeOwned;
use serde::Serialize;

use std::fmt::{self, Debug, Display};

use vclock::{VClock, Actor};
use ctx::{ReadCtx, AddCtx};
use traits::{Causal, CmRDT, CvRDT};

/// A Trait alias for the possible values MVReg's may hold
pub trait Val: Debug + Clone + Send + Serialize + DeserializeOwned {}
impl<T: Debug + Clone + Send + Serialize + DeserializeOwned> Val for T {}

/// MVReg (Multi-Value Register)
/// On concurrent writes, we will keep all values for which
/// we can't establish a causal history.
///
/// ```rust
/// use crdts::{CmRDT, MVReg, Dot, VClock};
/// let mut r1 = MVReg::<String, u8>::new();
/// let mut r2 = r1.clone();
/// let r1_read_ctx = r1.read();
/// let r2_read_ctx = r2.read();
///
/// let op1 = r1.set("bob", r1_read_ctx.derive_add_ctx(123));
/// r1.apply(&op1);
///
/// let op2 = r2.set("alice", r2_read_ctx.derive_add_ctx(111));
/// r2.apply(&op2);
///
/// r1.apply(&op2); // we replicate op2 to r1
/// 
/// let read_ctx = r1.read();
/// assert_eq!(read_ctx.val, vec!["bob".to_string(), "alice".to_string()]);
/// assert_eq!(
///     read_ctx.add_clock,
///     vec![(123, 1), (111, 1)]
///       .into_iter()
///       .collect()
/// );
/// ```
#[serde(bound(deserialize = ""))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MVReg<V: Val, A: Actor> {
    vals: Vec<(VClock<A>, V)>
}

/// Defines the set of operations over the MVReg
#[serde(bound(deserialize = ""))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op<V: Val, A: Actor> {
    /// Put a value
    Put {
        /// context of the operation
        clock: VClock<A>,
        /// the value to put
        val: V
    }
}

impl<V: Val + Display, A: Actor + Display> Display for MVReg<V, A> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "|")?;
        for (i, (ctx, val)) in self.vals.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{}@{}", val, ctx)?;
        }
        write!(f, "|")
    }
}

impl<V: Val + PartialEq, A: Actor> PartialEq for MVReg<V, A> {
    fn eq(&self, other: &Self) -> bool {
        for dot in self.vals.iter() {
            let mut num_found = other.vals.iter().filter(|d| d == &dot).count();

            if num_found == 0 {
                return false
            }
            // sanity check
            assert_eq!(num_found, 1);
        }
        for dot in other.vals.iter() {
            let mut num_found = self.vals.iter().filter(|d| d == &dot).count();

            if num_found == 0 {
                return false
            }
            // sanity check
            assert_eq!(num_found, 1);
        }
        true
    }
}

impl<V: Val + Eq, A: Actor> Eq for MVReg<V, A> {}

impl<V: Val, A: Actor> Causal<A> for MVReg<V, A> {
    fn truncate(&mut self, clock: &VClock<A>) {
        self.vals = self.vals.clone().into_iter()
            .filter_map(|(mut val_clock, val)| {
                val_clock.subtract(&clock);
                if val_clock.is_empty() {
                    None
                } else {
                    Some((val_clock, val))
                }
            })
            .collect()
    }
}

impl<V: Val, A: Actor> Default for MVReg<V, A> {
    fn default() -> Self {
        MVReg { vals: Vec::new() }
    }
}

impl<V: Val, A: Actor> CvRDT for MVReg<V, A> {
    fn merge(&mut self, other: &Self) {
        let mut vals = Vec::new();
        for (clock, val) in self.vals.iter() {
            let num_dominating = other.vals
                .iter()
                .filter(|(c, _)| clock < c)
                .count();
            if num_dominating == 0 {
                vals.push((clock.clone(), val.clone()));
            }
        }
        for (clock, val) in other.vals.iter() {
            let num_dominating = self.vals
                .iter()
                .filter(|(c, _)| clock < c)
                .count();
            if num_dominating == 0 {
                let mut is_new = true;
                for (existing_c, _) in vals.iter() {
                    if existing_c == clock {
                        is_new = false;
                        break;
                    }
                }
                if is_new {
                    vals.push((clock.clone(), val.clone()));
                }
            }
        }
        self.vals = vals;
    }
}

impl<V: Val, A: Actor> CmRDT for MVReg<V, A> {
    type Op = Op<V, A>;

    fn apply(&mut self, op: &Self::Op) {
        match op.clone() {
            Op::Put { clock, val } => {
                if clock.is_empty() {
                    return;
                }
                // first filter out all values that are dominated by the Op clock
                self.vals.retain(|(val_clock, _)| !(val_clock <= &clock));

                // TAI: in the case were the Op has a context that already was present,
                //      the above line would remove that value, the next lines would
                //      keep the val from the Op, so.. a malformed Op could break
                //      comutativity.
                
                // now check if we've already seen this op
                let mut should_add = true;
                for (existing_clock, _) in self.vals.iter() {
                    if existing_clock > &clock {
                        // we've found an entry that dominates this op
                        should_add = false;
                    }
                }

                if should_add {
                    self.vals.push((clock, val));
                }
            }
        }
    }
}

impl<V: Val, A: Actor> MVReg<V, A> {
    /// Construct a new empty MVReg
    pub fn new() -> Self {
        MVReg::default()
    }

    /// Set the value of the register
    pub fn set(&self, val: impl Into<V>, ctx: AddCtx<A>) -> Op<V, A> {
        Op::Put { clock: ctx.clock, val: val.into() }
    }

    /// Consumes the register and returns the values
    pub fn read(&self) -> ReadCtx<Vec<V>, A> {
        let clock = self.clock().clone();
        let concurrent_vals = self.vals
            .iter()
            .cloned()
            .map(|(_, v)| v)
            .collect();
        ReadCtx {
            add_clock: clock.clone(),
            rm_clock: clock,
            val: concurrent_vals
        }
    }

    /// A clock with latest versions of all actors operating on this register
    fn clock(&self) -> VClock<A> {
        self.vals.iter()
            .fold(VClock::new(), |mut accum_clock, (c, _)| {
                accum_clock.merge(&c);
                accum_clock
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fmt;
    use quickcheck::{Arbitrary, Gen, TestResult};

    use vclock::Dot;

    #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
    struct TActor(u8);
    
    #[derive(Debug, Clone)]
    struct TestReg<V: Val, A: Actor> {
        reg: MVReg<V, A>,
        ops: Vec<Op<V, A>>
    }

    impl<V: Val + Eq, A: Actor> TestReg<V, A> {
        fn incompat(&self, other: &Self) -> bool {
            for (c1, v1) in self.reg.vals.iter() {
                for (c2, v2) in other.reg.vals.iter() {
                    if c1 == c2 && v1 != v2 {
                        return true;
                    }
                }
            }

            for Op::Put { clock: c, val: v } in self.ops.iter() {
                for Op::Put { clock: other_c, val: other_v } in other.ops.iter() {
                    if c == other_c && v != other_v {
                        return true;
                    }
                }
            }

            return false;
        }
    }

    impl Arbitrary for TActor {
        fn arbitrary<G: Gen>(g: &mut G) -> Self {
            let actor: u8 = g.gen_range(0, 10);
            TActor(actor)
        }
        fn shrink(&self) -> Box<Iterator<Item = Self>> {
            Box::new(vec![].into_iter())
        }
    }

    impl Debug for TActor {
        fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            write!(formatter, "A{}", self.0)
        }
    }
    
    impl<V: Val + Arbitrary, A: Actor + Arbitrary>  Arbitrary for TestReg<V, A> {
        fn arbitrary<G: Gen>(g: &mut G) -> Self {
            let mut reg: MVReg<V, A> = MVReg::default();
            let num_ops = g.gen::<u8>() % 20;
            let mut ops = Vec::with_capacity(num_ops as usize);
            for _ in 0..num_ops {
                let val = V::arbitrary(g);
                let actor = A::arbitrary(g);
                let ctx = reg.read().derive_add_ctx(actor);
                let op = reg.set(val, ctx);
                reg.apply(&op);
                ops.push(op);
            }
            TestReg { reg, ops }
        }

        fn shrink(&self) -> Box<Iterator<Item = Self>> {
            let mut shrunk = vec![];

            for i in (0..self.ops.len()).into_iter().rev() {
                let mut reg = MVReg::new();
                let mut ops = Vec::with_capacity(self.ops.len() - 1);
                
                for (j, op) in self.ops.iter().cloned().enumerate() {
                    if i == j {
                        continue;
                    }

                    reg.apply(&op);
                    ops.push(op);
                }

                shrunk.push(TestReg { reg, ops });
            }
            Box::new(shrunk.into_iter())
        }
    }

    #[test]
    fn test_apply() {
        let mut reg = MVReg::new();
        let clock = VClock::from(Dot { actor: 2, counter: 1 });
        reg.apply(&Op::Put { clock: clock.clone(), val: 71 });
        assert_eq!(reg, MVReg { vals: vec![(clock, 71)] });
    }

    #[test]
    fn test_set_should_not_mutate_reg() {
        let reg = MVReg::<u8, u8>::new();
        let ctx = reg.read().derive_add_ctx(1);
        let op = reg.set(32, ctx);
        assert_eq!(reg, MVReg::new());
        let mut reg = reg;
        reg.apply(&op);

        let read_ctx = reg.read();
        assert_eq!(read_ctx.val, vec![32]);
        assert_eq!(read_ctx.add_clock, VClock::from(Dot { actor: 1, counter: 1 }));
    }

    #[test]
    fn test_concurrent_update_with_same_value_dont_collapse_on_merge() {
        // this is important to prevent because it breaks commutativity
        let mut r1: MVReg<u8, u8> = MVReg::new();
        let mut r2 = MVReg::new();

        let ctx_4 = r1.read().derive_add_ctx(4);
        let ctx_7 = r2.read().derive_add_ctx(7);

        let op1 = r1.set(23, ctx_4);
        let op2 = r2.set(23, ctx_7);
        r1.apply(&op1);
        r2.apply(&op2);

        r1.merge(&r2);

        let read_ctx = r1.read();
        assert_eq!(read_ctx.val, vec![23, 23]);
        assert_eq!(
            read_ctx.add_clock,
            VClock::from(vec![(4, 1), (7, 1)])
        );
    }

    #[test]
    fn test_concurrent_update_with_same_value_dont_collapse_on_apply() {
        // this is important to prevent because it breaks commutativity
        let mut r1: MVReg<u8, u8> = MVReg::new();
        let r2 = MVReg::new();

        let ctx_4 = r1.read().derive_add_ctx(4);
        let ctx_7 = r2.read().derive_add_ctx(7);

        let op1 = r1.set(23, ctx_4);
        r1.apply(&op1);
        let op2 = r2.set(23, ctx_7);
        r1.apply(&op2);

        let read_ctx = r1.read();
        assert_eq!(read_ctx.val, vec![23, 23]);
        assert_eq!(
            read_ctx.add_clock,
            VClock::from(vec![(4, 1), (7, 1)])
        );
    }

    #[test]
    fn test_multi_val() {
        let mut r1 = MVReg::<u8, u8>::new();
        let mut r2 = MVReg::<u8, u8>::new();
        
        let ctx_1 = r1.read().derive_add_ctx(1);
        let ctx_2 = r2.read().derive_add_ctx(2);

        let op1 = r1.set(32, ctx_1);
        let op2 = r2.set(82, ctx_2);

        r1.apply(&op1);
        r2.apply(&op2);

        r1.merge(&r2);
        let read_ctx = r1.read();
        
        assert!(
            read_ctx.val == vec![32, 82] ||
                read_ctx.val == vec![82, 32]
        );
    }

    #[test]
    fn test_op_commute_quickcheck1() {
        let mut reg1 = MVReg::new();
        let mut reg2 = MVReg::new();

        let op1 = Op::Put { clock: Dot { actor: 1, counter: 1 }.into(), val: 1 };
        let op2 = Op::Put { clock: Dot { actor: 2, counter: 1 }.into(), val: 2 };

        reg2.apply(&op2);
        reg2.apply(&op1);
        reg1.apply(&op1);
        reg1.apply(&op2);

        assert_eq!(reg1, reg2);
    }

    quickcheck! {
        fn prop_sanity_check_arbitrary(r: TestReg<u8, TActor>) -> bool {
            let mut reg = MVReg::new();
            for op in r.ops.iter() {
                reg.apply(op);
            }

            assert_eq!(reg, r.reg);
            true
        }

        fn prop_set_with_ctx_from_read(r: TestReg<u8, TActor>, a: TActor) -> bool {
            let mut reg = r.reg;
            let write_ctx = reg.read().derive_add_ctx(a);
            let op = reg.set(23, write_ctx);
            reg.apply(&op);

            let next_read_ctx = reg.read();
            next_read_ctx.val == vec![23]
        }
        
        fn prop_merge_idempotent(r: TestReg<u8, TActor>) -> bool {
            let mut r = r.reg;
            let r_snapshot = r.clone();

            r.merge(&r_snapshot);

            assert_eq!(r, r_snapshot);
            true
        }

        fn prop_merge_commutative(r1: TestReg<u8, TActor>, r2: TestReg<u8, TActor>) -> TestResult {
            if r1.incompat(&r2) {
                return TestResult::discard();
            }
            let mut r1 = r1.reg;
            let mut r2 = r2.reg;

            let r1_snapshot = r1.clone();
            r1.merge(&r2);
            r2.merge(&r1_snapshot);

            assert_eq!(r1, r2);
            TestResult::from_bool(true)
        }

        fn prop_merge_associative(r1: TestReg<u8, TActor>, r2: TestReg<u8, TActor>, r3: TestReg<u8, TActor>) -> TestResult {
            if r1.incompat(&r2) || r1.incompat(&r3) || r2.incompat(&r3) {
                return TestResult::discard();
            }
            let mut r1 = r1.reg;
            let mut r2 = r2.reg;
            let r3 = r3.reg;
            let r1_snapshot = r1.clone();
            
            // r1 ^ r2
            r1.merge(&r2);

            // (r1 ^ r2) ^ r3
            r1.merge(&r3);

            // r2 ^ r3
            r2.merge(&r3);

            // r1 ^ (r2 ^ r3)
            r2.merge(&r1_snapshot);

            assert_eq!(r1, r2);
            TestResult::from_bool(true)
        }

        fn prop_truncate(r: TestReg<u8, TActor>) -> bool{
            let mut r = r.reg;
            let r_snapshot = r.clone();

            // truncating with the empty clock should be a nop
            r.truncate(&VClock::new());
            assert_eq!(r, r_snapshot);

            // truncating with the merge of all val clocks should give us
            // an empty register
            let clock = r.vals
                .iter()
                .fold(VClock::new(), |mut accum_clock, (c, _)| {
                    accum_clock.merge(&c);
                    accum_clock
                });

            r.truncate(&clock);
            assert_eq!(r, MVReg::new());
            true
        }

        fn prop_op_idempotent(test: TestReg<u8, TActor>) -> TestResult {
            let mut r = test.reg;
            let r_snapshot = r.clone();
            for op in test.ops.iter() {
                r.apply(op);
            }

            assert_eq!(r, r_snapshot);
            TestResult::from_bool(true)
        }

        fn prop_op_commutative(o1: TestReg<u8, TActor>, o2: TestReg<u8, TActor>) -> TestResult {
            if o1.incompat(&o2) {
                return TestResult::discard();
            }

            let mut r1 = o1.reg;
            let mut r2 = o2.reg;

            for op in o2.ops.iter() {
                r1.apply(op);
            }
            
            for op in o1.ops.iter() {
                r2.apply(op);
            }

            assert_eq!(r1, r2);
            TestResult::from_bool(true)
        }

        fn prop_op_associative(o1: TestReg<u8, TActor>, o2: TestReg<u8, TActor>, o3: TestReg<u8, TActor>) -> TestResult {
            if o1.incompat(&o2) || o1.incompat(&o3) || o2.incompat(&o3) {
                return TestResult::discard();
            }

            let mut r1 = o1.reg;
            let mut r2 = o2.reg;


            // r1 <- r2
            for op in o2.ops.iter() {
                r1.apply(op);
            }

            // (r1 <- r2) <- r3
            for op in o3.ops.iter() {
                r1.apply(op);
            }

            // r2 <- r3
            for op in o3.ops.iter() {
                r2.apply(op);
            }

            // (r2 <- r3) <- r1
            for op in o1.ops.iter() {
                r2.apply(op);
            }

            assert_eq!(r1, r2);
            TestResult::from_bool(true)
        }
    }
}