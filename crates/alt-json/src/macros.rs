/// Builds a [`Value`](crate::Value) from JSON-like syntax.
///
/// ```
/// let name = "alt";
/// let value = alt_json::json!({
///     "name": name,
///     "tags": ["a", "b", null],
///     "nested": { "ok": true, "count": 1 + 2 },
/// });
/// assert_eq!(
///     alt_json::to_string(&value).unwrap(),
///     r#"{"name":"alt","nested":{"count":3,"ok":true},"tags":["a","b",null]}"#
/// );
/// ```
///
/// Any expression whose type implements [`ToJson`](crate::ToJson) can be
/// used as an element, a member value or the whole argument; it is borrowed,
/// not moved. The macro panics if such a value cannot be represented, for
/// example a map whose keys are not strings. Keys are string literals, identifiers bound to something that
/// converts into `String`, or any such expression in parentheses. Trailing
/// commas are allowed.
#[macro_export]
macro_rules! json {
    (null) => {
        $crate::Value::Null
    };
    ([]) => {
        $crate::Value::Array(::std::vec::Vec::new())
    };
    ([ $($tt:tt)+ ]) => {
        $crate::Value::Array($crate::__json_array!([] $($tt)+))
    };
    ({}) => {
        $crate::Value::Object($crate::Map::new())
    };
    ({ $($tt:tt)+ }) => {{
        let mut object = $crate::Map::new();
        $crate::__json_object!(object () ($($tt)+) ($($tt)+));
        $crate::Value::Object(object)
    }};
    ($other:expr) => {
        $crate::to_value(&$other).unwrap()
    };
}

// `[done elements] rest`: moves one element at a time from the rest into the
// bracketed list. null, arrays and objects are matched as tokens before the
// expression rules, because they are not valid expressions.
#[macro_export]
#[doc(hidden)]
macro_rules! __json_array {
    ([$($elems:expr,)*]) => {
        ::std::vec![$($elems,)*]
    };
    ([$($elems:expr),*]) => {
        ::std::vec![$($elems),*]
    };
    ([$($elems:expr,)*] null $($rest:tt)*) => {
        $crate::__json_array!([$($elems,)* $crate::json!(null)] $($rest)*)
    };
    ([$($elems:expr,)*] [$($array:tt)*] $($rest:tt)*) => {
        $crate::__json_array!([$($elems,)* $crate::json!([$($array)*])] $($rest)*)
    };
    ([$($elems:expr,)*] {$($map:tt)*} $($rest:tt)*) => {
        $crate::__json_array!([$($elems,)* $crate::json!({$($map)*})] $($rest)*)
    };
    ([$($elems:expr,)*] $next:expr, $($rest:tt)*) => {
        $crate::__json_array!([$($elems,)* $crate::json!($next),] $($rest)*)
    };
    ([$($elems:expr,)*] $last:expr) => {
        $crate::__json_array!([$($elems,)* $crate::json!($last)])
    };
    ([$($elems:expr),*] , $($rest:tt)*) => {
        $crate::__json_array!([$($elems,)*] $($rest)*)
    };
}

// `object (key tokens so far) (rest) (copy of rest)`: collects key tokens up
// to the colon, then the value, then inserts. The copy lets a rule match the
// rest twice, once as tokens and once to pass it on unchanged.
#[macro_export]
#[doc(hidden)]
macro_rules! __json_object {
    ($object:ident () () ()) => {};

    // insert, with more entries after the comma
    ($object:ident [$($key:tt)+] ($value:expr) , $($rest:tt)*) => {
        let _ = $object.insert(($($key)+).into(), $value);
        $crate::__json_object!($object () ($($rest)*) ($($rest)*));
    };
    // insert the last entry
    ($object:ident [$($key:tt)+] ($value:expr)) => {
        let _ = $object.insert(($($key)+).into(), $value);
    };

    ($object:ident ($($key:tt)+) (: null $($rest:tt)*) $copy:tt) => {
        $crate::__json_object!($object [$($key)+] ($crate::json!(null)) $($rest)*);
    };
    ($object:ident ($($key:tt)+) (: [$($array:tt)*] $($rest:tt)*) $copy:tt) => {
        $crate::__json_object!($object [$($key)+] ($crate::json!([$($array)*])) $($rest)*);
    };
    ($object:ident ($($key:tt)+) (: {$($map:tt)*} $($rest:tt)*) $copy:tt) => {
        $crate::__json_object!($object [$($key)+] ($crate::json!({$($map)*})) $($rest)*);
    };
    ($object:ident ($($key:tt)+) (: $value:expr , $($rest:tt)*) $copy:tt) => {
        $crate::__json_object!($object [$($key)+] ($crate::json!($value)) , $($rest)*);
    };
    ($object:ident ($($key:tt)+) (: $value:expr) $copy:tt) => {
        $crate::__json_object!($object [$($key)+] ($crate::json!($value)));
    };

    // a parenthesised expression as the key
    ($object:ident () (($key:expr) : $($rest:tt)*) $copy:tt) => {
        $crate::__json_object!($object ($key) (: $($rest)*) (: $($rest)*));
    };
    // one more token of the key
    ($object:ident ($($key:tt)*) ($tt:tt $($rest:tt)*) $copy:tt) => {
        $crate::__json_object!($object ($($key)* $tt) ($($rest)*) ($($rest)*));
    };
}
