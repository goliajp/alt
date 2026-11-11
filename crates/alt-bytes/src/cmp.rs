use crate::BString;

macro_rules! eq_bytes {
    ($($lhs:ty, $rhs:ty;)*) => {$(
        impl PartialEq<$rhs> for $lhs {
            fn eq(&self, other: &$rhs) -> bool {
                AsRef::<[u8]>::as_ref(self) == AsRef::<[u8]>::as_ref(other)
            }
        }
    )*};
}

eq_bytes! {
    BString, [u8];
    BString, &str;
    BString, &[u8];
}
