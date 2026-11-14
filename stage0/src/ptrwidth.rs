//! ターゲットの「ポインタの幅」(`usize` / `isize` / 生ポインタ / 参照の幅、ビット)。修正プラン 4-2 / 5-3。
//!
//! 既定は 64 ビット (x86_64・aarch64 の Linux)。`ksn build --target wasm32-unknown-unknown` と `ksn check --target wasm32-unknown-unknown` は
//! 32 ビットにする。この幅は KIR の型の幅 (`Ty::bits`)、集成体の大きさ (`kir::size_of`)、リテラルの範囲検査 (`types.rs`)、
//! LLVM IR の型 (`kir_llvm.rs`) が読む。スレッドごとの値なので、ビルドの入口 (`cli.rs`) が処理の前に一度だけ決める。
//! 既定の 64 のままなら、これまでの出力と一致する。

use std::cell::Cell;

thread_local! {
    static BITS: Cell<u32> = const { Cell::new(64) };
}

/// 今のターゲットのポインタの幅 (ビット)。32 か 64。
pub fn bits() -> u32 {
    BITS.with(|b| b.get())
}

/// ポインタの幅を決める (32 か 64 のほかは 64 に直す)。
pub fn set_bits(n: u32) {
    BITS.with(|b| b.set(if n == 32 { 32 } else { 64 }));
}

/// ポインタの幅を今の値に固定して戻る守り。落ちたら元の幅に戻る (`ksn build` の入口で使う)。
pub struct Guard(u32);

impl Guard {
    pub fn set(n: u32) -> Guard {
        let old = bits();
        set_bits(n);
        Guard(old)
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        set_bits(self.0);
    }
}
