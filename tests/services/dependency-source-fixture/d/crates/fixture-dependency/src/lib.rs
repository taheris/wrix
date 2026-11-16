pub fn value() -> u32 {
    if cfg!(feature = "alternate") { 2 } else { 3 }
}
