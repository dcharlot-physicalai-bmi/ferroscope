fn main() {
    for phrase in [
        "drop a red crate from 2 m next to an SO-101 arm for 5 seconds",
        "three balls falling onto a conveyor belt",
        "a 30 cm aluminium cube orbiting at 0.8 m every 3 s",
        "two cylinders sliding back and forth, and a robot",
        "drop a ball on the moon",
        "make it look nice",
    ] {
        println!("\n>>> {phrase}");
        match ferroscope_phrase::read(phrase) {
            Ok(r) => {
                for u in &r.understood {
                    println!("  understood  {u}");
                }
                for a in &r.assumed {
                    println!("  assumed     {a}");
                }
                for g in &r.ignored {
                    println!("  IGNORED     {g:?}");
                }
            }
            Err(e) => println!("  REFUSED: {e}"),
        }
    }
}
