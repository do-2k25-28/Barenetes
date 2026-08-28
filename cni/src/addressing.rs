use std::net::Ipv4Addr;

pub(crate) fn gateway(vlan: u8, node: u8) -> Ipv4Addr {
    Ipv4Addr::new(10, vlan, node, 1)
}

pub(crate) fn pool_range(vlan: u8, node: u8) -> (Ipv4Addr, Ipv4Addr) {
    (
        Ipv4Addr::new(10, vlan, node, 2),
        Ipv4Addr::new(10, vlan, node, 254),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_the_gateway_for_a_tenant_and_node() {
        assert_eq!(gateway(100, 1), Ipv4Addr::new(10, 100, 1, 1));
        assert_eq!(gateway(200, 2), Ipv4Addr::new(10, 200, 2, 1));
    }

    #[test]
    fn computes_the_pool_range_for_a_tenant_and_node() {
        assert_eq!(
            pool_range(100, 1),
            (Ipv4Addr::new(10, 100, 1, 2), Ipv4Addr::new(10, 100, 1, 254))
        );
    }

    #[test]
    fn different_tenants_never_share_a_range() {
        let (first_a, last_a) = pool_range(100, 1);
        let (first_b, last_b) = pool_range(200, 1);
        assert_ne!(first_a, first_b);
        assert_ne!(last_a, last_b);
    }

    #[test]
    fn different_nodes_never_share_a_range() {
        let (first_node_1, _) = pool_range(100, 1);
        let (first_node_2, _) = pool_range(100, 2);
        assert_ne!(first_node_1, first_node_2);
    }
}
