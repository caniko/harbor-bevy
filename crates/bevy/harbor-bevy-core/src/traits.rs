/// Reset a value to its initial state. By default, use `Self::default()`.
pub trait Resettable: Default {
    fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::Resettable;

    #[test]
    fn resets_to_the_implementors_default() {
        struct Counter(u32);

        impl Default for Counter {
            fn default() -> Self {
                Self(10)
            }
        }

        impl Resettable for Counter {}

        let mut counter = Counter(42);
        counter.reset();
        assert_eq!(counter.0, 10);
    }
}
