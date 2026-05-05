use std::{fmt::Debug, str::FromStr};

pub fn get_var_t<T>(key: &str, default: T) -> T
where
    T: FromStr,
    <T as FromStr>::Err: Debug,
{
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse::<T>().ok())
        .unwrap_or(default)
}

#[macro_export]
macro_rules! env_lazy {
    ($( $vis:vis $name:ident : $ty:ty = ($key:literal, $default:expr); )* ) => {
        $(
            $vis static $name: ::std::sync::LazyLock<$ty> = ::std::sync::LazyLock::new(|| {
                dotenv::dotenv().ok();
                $crate::constants::get_var_t::<$ty>($key, $default)
            });
        )*
    };
}
