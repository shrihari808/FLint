const Corestore = require('corestore');
const fs = require('fs');
const path = require('path');
const { joinSwarm } = require('./swarm');

async function initFeed(storageDir) {
    const store = new Corestore(storageDir);
    const core = store.get({ name: 'flint-primary' });
    await core.ready();
    return core.key.toString('hex');
}

async function pushVersion(storageDir, filePath, note) {
    const store = new Corestore(storageDir);
    const core = store.get({ name: 'flint-primary' });
    await core.ready();
    const fileBuffer = fs.readFileSync(filePath);
    await core.append(Buffer.from(JSON.stringify({ note, timestamp: Date.now(), file: fileBuffer.toString('base64') })));
    const swarm = await joinSwarm(core);
    return { success: true, version: core.length };
}

async function pullLatest(storageDir, outDir) {
    const store = new Corestore(storageDir);
    const core = store.get({ name: 'flint-primary' });
    await core.ready();
    
    const swarm = await joinSwarm(core);
    await new Promise(r => setTimeout(r, 3000));
    
    if (core.length === 0) {
        throw new Error("Feed is empty");
    }
    const lastEntryBuffer = await core.get(core.length - 1);
    const lastEntry = JSON.parse(lastEntryBuffer.toString());
    const fileBuffer = Buffer.from(lastEntry.file, 'base64');
    
    if (!fs.existsSync(outDir)) {
        fs.mkdirSync(outDir, { recursive: true });
    }
    
    const outPath = path.join(outDir, 'project.zip');
    fs.writeFileSync(outPath, fileBuffer);
    
    return { success: true, file: outPath, note: lastEntry.note, version: core.length };
}

async function getLog(storageDir) {
    const store = new Corestore(storageDir);
    const core = store.get({ name: 'flint-primary' });
    await core.ready();
    
    const logs = [];
    for (let i = 0; i < core.length; i++) {
        const entryBuffer = await core.get(i);
        const entry = JSON.parse(entryBuffer.toString());
        logs.push({
            version: i + 1,
            timestamp: entry.timestamp || Date.now(),
            note: entry.note
        });
    }
    return logs;
}

async function getStatus(storageDir) {
    const store = new Corestore(storageDir);
    const core = store.get({ name: 'flint-primary' });
    await core.ready();
    
    const swarm = await joinSwarm(core);
    await new Promise(r => setTimeout(r, 3000));
    
    const peerOnline = core.peers.length > 0;
    const peerVersion = peerOnline ? Math.max(...core.peers.map(p => p.remoteLength || 0)) : 0;
    
    await swarm.destroy();
    
    return { peer_online: peerOnline, peer_version: peerVersion, local_version: core.length };
}

module.exports = {
    initFeed,
    pushVersion,
    pullLatest,
    getLog,
    getStatus
};
