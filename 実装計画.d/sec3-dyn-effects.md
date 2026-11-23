# sec3-dyn-effects

- H2: effects_conv の into_dyn 検査が `alloc.boxed.Box.into_dyn` のフルパス形と Def 解決を取りこぼしていた。into_dyn を match の外で判定し、`Coercion::Unsize` の全箇所も `walk` に通す。
- H1: `s.job.run()` の経路形の呼び先に、欄を辿った受け手の型を載せる (types.rs の RecvPath を 3 区間以上でも常に記録)。
