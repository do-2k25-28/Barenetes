use cni::ip_pool::IpPool;
use std::io;
use std::net::Ipv4Addr;

#[test]
fn allocates_persists_and_releases_addresses() {
    let directory = tempfile::tempdir().unwrap();
    let first = Ipv4Addr::new(10, 0, 0, 2);
    let last = Ipv4Addr::new(10, 0, 0, 3);
    let pool = IpPool::new(directory.path(), first, last).unwrap();

    assert_eq!(pool.allocate().unwrap(), first);
    assert_eq!(pool.allocate().unwrap(), last);
    assert!(pool.allocate().is_err());
    assert!(pool.release(first).unwrap());
    assert_eq!(
        IpPool::new(directory.path(), first, last)
            .unwrap()
            .allocate()
            .unwrap(),
        first
    );
}

#[test]
fn fails_closed_on_corrupt_state() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("ip-pool.json"), b"invalid").unwrap();
    let pool = IpPool::new(
        directory.path(),
        Ipv4Addr::new(10, 0, 0, 2),
        Ipv4Addr::new(10, 0, 0, 3),
    )
    .unwrap();

    assert_eq!(
        pool.allocate().unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}
