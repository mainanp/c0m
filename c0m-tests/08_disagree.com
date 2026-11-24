//Structurally "big" function, but nearly all its time is in one small loop.
// Expected: the loop starts at tier 1 by structure; measurement lifts it to tier 2.
fn busy() -> i64 {
    let mut t: i64 = 0;
    let a: i64 = 1; let b: i64 = 2; let c: i64 = 3; let d: i64 = 4;
    let e: i64 = 5; let f: i64 = 6; let g: i64 = 7; let h: i64 = 8;
    for i in 0..3000000 { t = t + i; }
    return t + a + b + c + d + e + f + g + h;
}
fn main() -> () { let r: i64 = busy(); }
