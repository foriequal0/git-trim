use std::ops::Deref;

/// Use with caution.
/// It makes wrapping type T to be Send + Sync.
/// Make sure T is semantically Send + Sync
#[derive(Copy, Clone)]
pub struct ForceSendSync<T>(T);

unsafe impl<T> Sync for ForceSendSync<T> {}
unsafe impl<T> Send for ForceSendSync<T> {}

impl<T> ForceSendSync<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }
    pub fn unwrap(self) -> T {
        self.0
    }
}

impl<'a, T> ForceSendSync<&'a T> {
    pub fn as_static(self) -> ForceSendSync<&'static T> {
        unsafe { ForceSendSync(std::mem::transmute::<&'a T, &'static T>(self.0)) }
    }
}

impl<T> Deref for ForceSendSync<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
