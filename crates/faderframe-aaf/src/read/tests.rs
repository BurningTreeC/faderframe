use super::*;

#[test]
fn edit_units_rescale_rounding_to_the_nearest() {
    assert_eq!(rescale(25, Rational::new(25, 1), 48_000), 48_000);
    assert_eq!(rescale(1, Rational::new(30_000, 1001), 48_000), 1602);
    assert_eq!(
        convert(48_000, Rational::new(48_000, 1), Rational::new(25, 1)),
        25
    );
    assert_eq!(convert(-3, Rational::new(1, 1), Rational::new(2, 1)), -6);
}

#[test]
fn values_read_from_indirect_numbers() {
    let mut b = vec![0x4c];
    b.extend_from_slice(&model::ty::RATIONAL.to_le());
    b.extend_from_slice(&Rational::new(1, 4).encode());
    assert_eq!(indirect_number(&b), Some(0.25));
    let mut i = vec![0x4c];
    i.extend_from_slice(&crate::ids::id(0x01010700_0000_0000_060e_2b3401040101).to_le());
    i.extend_from_slice(&(-7i32).to_le_bytes());
    assert_eq!(indirect_number(&i), Some(-7.0));
}

#[test]
fn envelopes_multiply() {
    let a = [(0, 0.0), (100, -6.0206)];
    let b = [(50, -6.0206)];
    let c = combine(&a, &b);
    assert_eq!(c.len(), 3);
    assert!((c[0].1 + 6.0206).abs() < 1e-3);
    assert!((c[2].1 + 12.04).abs() < 1e-2);
}

#[test]
fn omf_files_are_told_apart() {
    let path = std::env::temp_dir().join(format!("ff-aaf-omf-{}.bin", std::process::id()));
    let mut b = vec![0u8; 300];
    b.extend_from_slice(b"\xa4CM\xa5Hdr\xd7");
    b.extend_from_slice(&[0; 16]);
    std::fs::write(&path, &b).unwrap();
    assert!(matches!(AafFile::open_path(&path), Err(ReadError::Omf)));
    std::fs::write(&path, b"RIFF....WAVE").unwrap();
    assert!(matches!(
        AafFile::open_path(&path),
        Err(ReadError::Cfb(CfbError::NotCompound))
    ));
    let _ = std::fs::remove_file(&path);
}
