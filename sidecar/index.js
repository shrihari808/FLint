const minimist = require('minimist');
const path = require('path');
const { initFeed, pushVersion, pullLatest, getLog, getStatus } = require('./feed');

const args = minimist(process.argv.slice(2));
const command = args._[0];

function outputJSON(data) {
    console.log(JSON.stringify(data));
}

async function handleInit(args) {
    try {
        const storageDir = path.resolve(__dirname, 'storage');
        const feedKey = await initFeed(storageDir);
        outputJSON({
            session_id: feedKey.slice(0, 6),
            feed_key: feedKey
        });
    } catch (err) {
        outputJSON({ success: false, error: err.message });
    }
}

async function handlePush(args) {
    try {
        const sessionId = args.session;
        const filePath = args.file;
        const note = args.note;
        if (!sessionId || !filePath || !note) {
            throw new Error("Missing --session, --file, or --note");
        }
        const storageDir = path.resolve(__dirname, 'storage');
        const result = await pushVersion(storageDir, filePath, note);
        outputJSON(result);
    } catch (err) {
        outputJSON({ success: false, error: err.message });
    }
}

async function handlePull(args) {
    try {
        const sessionId = args.session;
        const outDir = args.out;
        if (!sessionId || !outDir) {
            throw new Error("Missing --session or --out");
        }
        const storageDir = path.resolve(__dirname, 'storage');
        const resolvedOutDir = path.resolve(process.cwd(), outDir);
        const result = await pullLatest(storageDir, resolvedOutDir);
        outputJSON(result);
    } catch (err) {
        outputJSON({ success: false, error: err.message });
    }
}

async function handleLog(args) {
    try {
        const sessionId = args.session;
        if (!sessionId) {
            throw new Error("Missing --session");
        }
        const storageDir = path.resolve(__dirname, 'storage');
        const logs = await getLog(storageDir);
        outputJSON(logs);
    } catch (err) {
        outputJSON({ success: false, error: err.message });
    }
}

async function handleStatus(args) {
    try {
        const sessionId = args.session;
        if (!sessionId) {
            throw new Error("Missing --session");
        }
        const storageDir = path.resolve(__dirname, 'storage');
        const status = await getStatus(storageDir);
        outputJSON(status);
    } catch (err) {
        outputJSON({ success: false, error: err.message });
    }
}

async function main() {
    switch (command) {
        case 'init':
            await handleInit(args);
            break;
        case 'push':
            await handlePush(args);
            break;
        case 'pull':
            await handlePull(args);
            break;
        case 'log':
            await handleLog(args);
            break;
        case 'status':
            await handleStatus(args);
            break;
        default:
            outputJSON({ success: false, error: `Unknown command: ${command}` });
            break;
    }
}

main().catch(err => outputJSON({ success: false, error: err.message }));
