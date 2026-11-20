use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;

use crate::{Arch, Cell, CellId, CellName, CellSpec, CellState, ImagePin, Kind, LimaOptions, Mount, NetworkName, Port, Refusal, RefusalCode, Resources};
use proptest::prelude::*;

use crate::model::tree::MAX_DEPTH;
use crate::policy::*;

fn name(s: &str) -> CellName {
    s.parse().unwrap()
}

fn spec(n: &str, wall: &str) -> CellSpec {
    CellSpec {
        name: name(n),
        kind: Kind::Lima(LimaOptions::new(ImagePin::new("https://example.test/a.img".into(), format!("sha256:{}", "0".repeat(64)), Arch::Aarch64).unwrap())),
        parent: None,
        wall: wall.parse().unwrap(),
        resources: Resources::default(),
        ports: vec![],
        mounts: vec![],
        sealed: false,
        network: None,
        clone_from: None,
    }
}

fn cell(n: &str, wall: &str) -> Cell {
    Cell { id: CellId::from_bytes([0; 16]), spec: spec(n, wall), state: CellState::Stopped }
}

fn port(guest: u16, bind: IpAddr) -> Port {
    Port { guest, host: guest + 10000, bind }
}

fn mount(src: &str) -> Mount {
    Mount { source: PathBuf::from(src), target: "/mnt/x".into(), writable: false }
}

fn code<T>(r: Result<T, Refusal>) -> RefusalCode {
    r.err().expect("should be refused").code
}

#[test]
fn ports_on_other_addresses_are_refused() {
    let mut s = spec("adytum-a", "wall_a");
    s.ports = vec![port(80, Ipv4Addr::UNSPECIFIED.into())];
    assert_eq!(code(check_spec(&s)), RefusalCode::PortNotLocalhost);
    s.ports = vec![port(80, Ipv4Addr::new(192, 168, 1, 5).into())];
    assert_eq!(code(check_spec(&s)), RefusalCode::PortNotLocalhost);
    s.ports = vec![port(80, Ipv6Addr::UNSPECIFIED.into())];
    assert_eq!(code(check_spec(&s)), RefusalCode::PortNotLocalhost);
}

#[test]
fn loopback_ports_pass() {
    let mut s = spec("adytum-a", "wall_a");
    s.ports = vec![port(80, Ipv4Addr::LOCALHOST.into()), port(81, Ipv4Addr::new(127, 5, 5, 5).into()), port(82, Ipv6Addr::LOCALHOST.into())];
    assert!(check_spec(&s).is_ok());
}

#[test]
fn sealed_cell_with_a_mount_is_refused() {
    let mut s = spec("adytum-a", "wall_a");
    s.sealed = true;
    s.mounts = vec![mount("/work/a")];
    assert_eq!(code(check_spec(&s)), RefusalCode::SealedCellMount);
}

#[test]
fn mounts_are_fine_unless_sealed() {
    let mut s = spec("adytum-a", "wall_a");
    s.mounts = vec![mount("/work/a")];
    assert!(check_spec(&s).is_ok());
    s.mounts.clear();
    s.sealed = true;
    assert!(check_spec(&s).is_ok());
}

#[test]
fn a_taken_name_is_refused() {
    let s = spec("adytum-a", "wall_a");
    let w = vec![cell("adytum-a", "wall_a")];
    assert_eq!(code(check_create(&s, &w)), RefusalCode::NameTaken);
}

#[test]
fn a_free_name_passes() {
    let s = spec("adytum-b", "wall_a");
    assert!(check_create(&s, &[cell("adytum-a", "wall_a")]).is_ok());
}

#[test]
fn other_walls_cannot_share_a_network() {
    let mut s = spec("adytum-b", "wall_b");
    s.network = Some("net-1".parse().unwrap());
    let mut theirs = cell("adytum-a", "wall_a");
    theirs.spec.network = Some("net-1".parse().unwrap());
    assert_eq!(code(check_create(&s, &[theirs])), RefusalCode::WallNetwork);
}

#[test]
fn a_cell_with_no_network_is_never_refused_for_one() {
    let s = spec("adytum-b", "wall_b");
    let mut theirs = cell("adytum-a", "wall_a");
    theirs.spec.network = Some("net-1".parse().unwrap());
    assert!(check_create(&s, &[theirs]).is_ok());
}

#[test]
fn same_wall_may_share_a_network_and_other_networks_pass() {
    let mut s = spec("adytum-b", "wall_a");
    s.network = Some("net-1".parse().unwrap());
    let mut theirs = cell("adytum-a", "wall_a");
    theirs.spec.network = Some("net-1".parse().unwrap());
    assert!(check_create(&s, &[theirs.clone()]).is_ok());
    s.wall = "wall_b".parse().unwrap();
    theirs.spec.network = Some("net-2".parse().unwrap());
    assert!(check_create(&s, &[theirs]).is_ok());
}

#[test]
fn other_walls_cannot_share_a_mount_source() {
    let mut s = spec("adytum-b", "wall_b");
    s.mounts = vec![mount("/work/x")];
    let mut theirs = cell("adytum-a", "wall_a");
    theirs.spec.mounts = vec![mount("/work/x")];
    assert_eq!(code(check_create(&s, &[theirs])), RefusalCode::WallMount);
}

#[test]
fn a_mount_inside_or_around_another_walls_mount_is_shared() {
    let mut theirs = cell("adytum-a", "wall_a");
    theirs.spec.mounts = vec![mount("/work/x")];
    let w = vec![theirs];
    for src in ["/work/x/sub", "/work", "/work/x/../x/sub", "/work/./x"] {
        let mut s = spec("adytum-b", "wall_b");
        s.mounts = vec![mount(src)];
        assert_eq!(code(check_create(&s, &w)), RefusalCode::WallMount, "{src}");
    }
}

#[test]
fn unrelated_or_same_wall_mounts_pass() {
    let mut theirs = cell("adytum-a", "wall_a");
    theirs.spec.mounts = vec![mount("/work/x")];
    let w = vec![theirs];
    let mut s = spec("adytum-b", "wall_b");
    for src in ["/work/xy", "/other", "/work/y/../z"] {
        s.mounts = vec![mount(src)];
        assert!(check_create(&s, &w).is_ok(), "{src}");
    }
    s.wall = "wall_a".parse().unwrap();
    s.mounts = vec![mount("/work/x")];
    assert!(check_create(&s, &w).is_ok());
}

#[test]
fn cloning_across_walls_is_refused() {
    let mut s = spec("adytum-b", "wall_b");
    s.clone_from = Some(name("adytum-a"));
    let w = vec![cell("adytum-a", "wall_a")];
    assert_eq!(code(check_create(&s, &w)), RefusalCode::WallClone);
}

#[test]
fn cloning_within_a_wall_passes() {
    let mut s = spec("adytum-b", "wall_a");
    s.clone_from = Some(name("adytum-a"));
    let w = vec![cell("adytum-a", "wall_a")];
    assert!(check_create(&s, &w).is_ok());
}

#[test]
fn cloning_from_the_same_wall_or_from_the_image_passes() {
    let mut s = spec("adytum-b", "wall_a");
    s.clone_from = Some(name("adytum-a"));
    assert!(check_create(&s, &[cell("adytum-a", "wall_a")]).is_ok());
    s.clone_from = None;
    assert!(check_create(&s, &[]).is_ok());
}

#[test]
fn check_create_passes_a_clean_spec_and_reports_the_first_refusal() {
    let mut s = spec("adytum-b", "wall_a");
    assert!(check_create(&s, &[]).is_ok());
    // Both the port rule and the name rule would refuse: the port rule comes first.
    s.ports = vec![port(80, Ipv4Addr::UNSPECIFIED.into())];
    let w = vec![cell("adytum-b", "wall_a")];
    assert_eq!(code(check_create(&s, &w)), RefusalCode::PortNotLocalhost);
    s.ports.clear();
    assert_eq!(code(check_create(&s, &w)), RefusalCode::NameTaken);
}

fn host(n: &str, wall: &str) -> Cell {
    let mut c = cell(n, wall);
    if let Kind::Lima(options) = &mut c.spec.kind {
        options.containers = true;
    }
    c
}

fn container(n: &str, wall: &str, parent: &str) -> CellSpec {
    let image = format!("docker.io/library/alpine@sha256:{}", "0".repeat(64)).parse().unwrap();
    CellSpec { kind: Kind::Container(crate::ContainerOptions { image }), parent: Some(name(parent)), ..spec(n, wall) }
}

#[test]
fn a_cell_in_a_parent_that_can_hold_it_passes() {
    let mut sealed_host = host("adytum-p", "wall_a");
    sealed_host.spec.sealed = true;
    let mut child = container("adytum-c", "wall_a", "adytum-p");
    child.sealed = true;
    assert!(check_create(&child, &[sealed_host]).is_ok());
    let unsealed_child = container("adytum-c", "wall_a", "adytum-p");
    assert!(check_create(&unsealed_child, &[host("adytum-p", "wall_a")]).is_ok());
    let mut sealed_child = unsealed_child.clone();
    sealed_child.sealed = true;
    assert!(check_create(&sealed_child, &[host("adytum-p", "wall_a")]).is_ok(), "a sealed child of an open parent");
}

#[test]
fn a_parent_that_is_not_recorded_is_refused() {
    assert_eq!(code(check_create(&container("adytum-c", "wall_a", "adytum-p"), &[])), RefusalCode::ParentMissing);
}

#[test]
fn a_child_behind_another_wall_than_its_parent_is_refused() {
    assert_eq!(code(check_create(&container("adytum-c", "wall_b", "adytum-p"), &[host("adytum-p", "wall_a")])), RefusalCode::ParentWall);
}

#[test]
fn a_parent_that_cannot_run_the_childs_kind_is_refused() {
    // A Lima cell without containers, a Lima cell in a Lima cell, a container in a container.
    let plain = cell("adytum-p", "wall_a");
    assert_eq!(code(check_create(&container("adytum-c", "wall_a", "adytum-p"), &[plain])), RefusalCode::ParentCannotHost);
    let vm_in_vm = CellSpec { parent: Some(name("adytum-p")), ..spec("adytum-c", "wall_a") };
    assert_eq!(code(check_create(&vm_in_vm, &[host("adytum-p", "wall_a")])), RefusalCode::ParentCannotHost);
    let inner = Cell { id: CellId::from_bytes([1; 16]), spec: container("adytum-c", "wall_a", "adytum-p"), state: CellState::Running };
    let refused = check_create(&container("adytum-d", "wall_a", "adytum-c"), &[host("adytum-p", "wall_a"), inner]);
    let r = refused.err().unwrap();
    assert_eq!(r.code, RefusalCode::ParentCannotHost);
    assert!(r.message.contains("container") && !r.fix.is_empty(), "{r:?}");
}

#[test]
fn a_sealed_parent_holds_only_sealed_cells() {
    let mut p = host("adytum-p", "wall_a");
    p.spec.sealed = true;
    assert_eq!(code(check_create(&container("adytum-c", "wall_a", "adytum-p"), &[p])), RefusalCode::ParentSealed);
}

#[test]
fn a_nested_cell_publishes_no_port_and_mounts_nothing_even_on_loopback() {
    let mut s = container("adytum-c", "wall_a", "adytum-p");
    s.ports = vec![port(80, Ipv4Addr::LOCALHOST.into())];
    assert_eq!(code(check_spec(&s)), RefusalCode::NestedPort);
    s.ports.clear();
    s.mounts = vec![mount("/work/a")];
    assert_eq!(code(check_spec(&s)), RefusalCode::NestedMount);
    assert_eq!(code(check_create(&s, &[host("adytum-p", "wall_a")])), RefusalCode::NestedMount, "before the parent is even looked at");
}

#[test]
fn a_cell_too_deep_is_refused() {
    // Hosts that could hold anything, three deep: the fourth level is refused.
    let mut cells = vec![host("adytum-l0", "wall_a")];
    for i in 1..MAX_DEPTH {
        let mut c = host(&format!("adytum-l{i}"), "wall_a");
        c.spec.parent = Some(name(&format!("adytum-l{}", i - 1)));
        cells.push(c);
    }
    let deepest = format!("adytum-l{}", MAX_DEPTH - 1);
    assert!(check_create(&container("adytum-c", "wall_a", &deepest), &cells).is_ok());
    let mut c = host("adytum-c", "wall_a");
    c.spec.parent = Some(name(&deepest));
    cells.push(c);
    assert_eq!(code(check_create(&container("adytum-d", "wall_a", "adytum-c"), &cells)), RefusalCode::TooDeep);
}

#[test]
fn a_child_asks_for_no_more_than_its_parent_has() {
    let p = host("adytum-p", "wall_a");
    let have = p.spec.resources;
    let mut s = container("adytum-c", "wall_a", "adytum-p");
    s.resources = have;
    assert!(check_create(&s, std::slice::from_ref(&p)).is_ok(), "as much as the parent is fine");
    for want in [Resources { cpus: have.cpus + 1, ..have }, Resources { memory_mib: have.memory_mib + 1, ..have }] {
        s.resources = want;
        let r = check_create(&s, std::slice::from_ref(&p)).err().unwrap();
        assert_eq!(r.code, RefusalCode::ChildTooBig);
        assert!(r.message.contains("adytum-p") && !r.fix.is_empty(), "{r:?}");
    }
    // A container's disk is the parent's own; it is not compared.
    s.resources = Resources { disk_gib: have.disk_gib * 10, ..have };
    assert!(check_create(&s, &[p]).is_ok());
}

#[test]
fn a_clone_is_made_where_its_source_runs() {
    let source = Cell { id: CellId::from_bytes([2; 16]), spec: container("adytum-c", "wall_a", "adytum-p"), state: CellState::Stopped };
    let cells = [host("adytum-p", "wall_a"), host("adytum-q", "wall_a"), source];
    let mut copy = container("adytum-d", "wall_a", "adytum-q");
    copy.clone_from = Some(name("adytum-c"));
    assert_eq!(code(check_create(&copy, &cells)), RefusalCode::CloneHost);
    copy.parent = Some(name("adytum-p"));
    assert!(check_create(&copy, &cells).is_ok());
    let mut from_machine = spec("adytum-e", "wall_a");
    from_machine.clone_from = Some(name("adytum-c"));
    assert_eq!(code(check_create(&from_machine, &cells)), RefusalCode::CloneHost);
}

proptest! {
    #[test]
    fn any_non_loopback_bind_is_refused(octets in any::<[u8; 4]>(), v6 in any::<[u16; 8]>(), use_v6 in any::<bool>()) {
        let ip: IpAddr = if use_v6 { Ipv6Addr::from(v6).into() } else { Ipv4Addr::from(octets).into() };
        prop_assume!(!ip.is_loopback());
        let mut s = spec("adytum-a", "wall_a");
        s.ports = vec![port(80, ip)];
        prop_assert_eq!(code(check_spec(&s)), RefusalCode::PortNotLocalhost);
        prop_assert_eq!(code(check_create(&s, &[])), RefusalCode::PortNotLocalhost);
    }

    #[test]
    fn different_walls_with_a_common_network_are_refused(
        a in "[a-z0-9]{1,20}",
        b in "[a-z0-9]{1,20}",
        net in "[a-z0-9-]{1,48}",
    ) {
        prop_assume!(a != b);
        let mut s = spec("adytum-new", &format!("wall_{a}"));
        let net: NetworkName = net.parse().unwrap();
        s.network = Some(net.clone());
        let mut theirs = cell("adytum-old", &format!("wall_{b}"));
        theirs.spec.network = Some(net);
        prop_assert_eq!(
            code(check_create(&s, &[theirs])),
            RefusalCode::WallNetwork
        );
    }

    #[test]
    fn a_valid_spec_into_an_empty_world_passes(
        suffix in "[a-z0-9][a-z0-9-]{0,45}[a-z0-9]",
        wall in "[a-z0-9_-]{1,64}",
        ports in proptest::collection::vec((1u16..9000, any::<u8>()), 0..4),
        sealed in any::<bool>(),
    ) {
        let mut s = spec(&format!("adytum-{suffix}"), &format!("wall_{wall}"));
        s.ports = ports
            .into_iter()
            .map(|(g, last)| port(g, Ipv4Addr::new(127, 0, 0, last).into()))
            .collect();
        s.sealed = sealed;
        if !sealed {
            s.mounts = vec![mount("/work/a")];
        }
        prop_assert!(check_create(&s, &[]).is_ok());
    }
}
