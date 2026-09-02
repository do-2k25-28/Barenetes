MASTER_IP=10.0.0.230
SLAVE_IP=10.0.0.231

scp target/release/api $MASTER_IP:~/barenetes-api
scp systemd/barenetes-api.service $MASTER_IP:~

scp target/release/scheduler $MASTER_IP:~/barenetes-scheduler
scp systemd/barenetes-scheduler.service $MASTER_IP:~

scp target/release/agent $SLAVE_IP:~/barenetes-agent
scp systemd/barenetes-agent.service $SLAVE_IP:~ 

scp target/release/cni $SLAVE_IP:~/barenetes-cni
scp systemd/barenetes-cni.service $SLAVE_IP:~ 
