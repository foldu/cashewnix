use std::{
    hash::{Hash, Hasher},
    ops::Deref,
    sync::Arc,
};

use ahash::HashSet;

// maybe add a leaky deduper?
// pros:
// - fasterer
// - funny
// cons:
// leaks and this thing accepts external urls from the network
// but they're signed anyway so malicious url spam probably isn't possible

#[derive(Default)]
pub struct Deduper<T>
where
    T: Hash + Eq,
{
    values: HashSet<Arc<T>>,
}

#[derive(Clone, Debug)]
pub struct Unique<T> {
    // maybe store ptr to parent Deduper to avoid errors?
    ptr: Arc<T>,
}

impl<T> Hash for Unique<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.ptr).hash(state)
    }
}

impl<T> Ord for Unique<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        Arc::as_ptr(&self.ptr).cmp(&Arc::as_ptr(&other.ptr))
    }
}

impl<T> PartialOrd for Unique<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Eq for Unique<T> {}

impl<T> PartialEq for Unique<T> {
    fn eq(&self, other: &Self) -> bool {
        Arc::as_ptr(&self.ptr) == Arc::as_ptr(&other.ptr)
    }
}

impl<T> Deref for Unique<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.ptr.deref()
    }
}

impl<T> Deduper<T>
where
    T: Hash + Eq,
{
    pub fn new() -> Self {
        Self {
            values: HashSet::with_hasher(ahash::RandomState::default()),
        }
    }

    pub fn get_or_insert(&mut self, value: T) -> Unique<T> {
        let arc = match self.values.get(&value) {
            Some(value) => value.clone(),
            None => {
                let arc = Arc::new(value);
                self.values.insert(arc.clone());

                arc
            }
        };
        Unique { ptr: arc }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deduper_works() {
        let mut deduper = Deduper::new();
        let thing = String::from("5");
        let dup = deduper.get_or_insert(thing.clone());
        let other = deduper.get_or_insert(thing.clone());

        assert_eq!(dup, other);

        assert_eq!(String::clone(&dup), thing);
    }
}
