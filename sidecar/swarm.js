const Hyperswarm = require('hyperswarm');

async function joinSwarm(core) {
    const swarm = new Hyperswarm();
    
    swarm.on('connection', (conn) => {
        core.replicate(conn);
    });
    
    swarm.join(core.discoveryKey);
    await swarm.flush();
    
    return swarm;
}

module.exports = { joinSwarm };
