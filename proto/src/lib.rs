// Commented out because these proto files are currently empty stubs.

// pub mod agent {
//     pub mod v1 {
//         tonic::include_proto!("agent.v1");
//     }
// }
//
// pub mod api {
//     pub mod v1 {
//         tonic::include_proto!("api.v1");
//     }
// }
//
// pub mod barectl {
//     pub mod v1 {
//         tonic::include_proto!("barectl.v1");
//     }
// }
//
// pub mod cni {
//     pub mod v1 {
//         tonic::include_proto!("cni.v1");
//     }
// }

pub mod scheduler {
    pub mod v1 {
        tonic::include_proto!("scheduler.v1");
    }
}

pub mod shared {
    pub mod v1 {
        tonic::include_proto!("shared.v1");
    }
}
