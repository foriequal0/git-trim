use std::backtrace::Backtrace;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::ops::{Deref, DerefMut, Try};

pub struct TracedResult<T>(::std::result::Result<T, TracedError>);

impl<T> TracedResult<T> {
    pub fn ok(v: T) -> TracedResult<T> {
        Self(::std::result::Result::Ok(v))
    }

    pub fn err(v: TracedError) -> TracedResult<T> {
        Self(::std::result::Result::Err(v))
    }
}

impl<T> Deref for TracedResult<T> {
    type Target = ::std::result::Result<T, TracedError>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> DerefMut for TracedResult<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[derive(Debug)]
pub struct TracedError {
    boxed: Box<dyn std::error::Error>,
    backtrace: Backtrace,
}

impl Error for TracedError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.boxed.source()
    }

    fn backtrace(&self) -> Option<&Backtrace> {
        Some(&self.backtrace)
    }
}

impl Display for TracedError {
    fn fmt(&self, f: &mut Formatter<'_>) -> ::std::fmt::Result {
        writeln!(f, "{}", self.boxed)?;
        writeln!(f, "Backtrace:")?;
        writeln!(f, "{}", self.backtrace)?;
        Ok(())
    }
}

impl<T> Try for TracedResult<T> {
    type Ok = T;
    type Error = TracedError;

    fn into_result(self) -> ::std::result::Result<Self::Ok, Self::Error> {
        self.0
    }

    fn from_ok(v: T) -> Self {
        Self(Ok(v))
    }

    fn from_error(v: Self::Error) -> Self {
        Self(Err(v))
    }
}

macro_rules! gen_err {
    ($x:ty) => {
        impl From<$x> for TracedError {
            fn from(v: $x) -> Self {
                TracedError {
                    boxed: Box::new(v) as Box<dyn Error>,
                    backtrace: Backtrace::capture(),
                }
            }
        }
    };
}

gen_err!(::std::io::Error);
gen_err!(::std::str::Utf8Error);
gen_err!(::git2::Error);
impl<'a> From<&str> for TracedError {
    fn from(v: &str) -> Self {
        TracedError {
            boxed: v.into(),
            backtrace: Backtrace::capture(),
        }
    }
}
