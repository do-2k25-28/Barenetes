pub mod agent {
    pub mod v1 {
        tonic::include_proto!("agent.v1");
    }
}

pub mod api {
    pub mod v1 {
        tonic::include_proto!("api.v1");
    }
}

pub mod cni {
    pub mod v1 {
        tonic::include_proto!("cni.v1");
    }
}

pub mod shared {
    pub mod v1 {
        tonic::include_proto!("shared.v1");
    }
}

pub mod tls;
