const minimist = require('minimist');

const args = minimist(process.argv.slice(2));
const command = args._[0];

function outputJSON(data) {
    console.log(JSON.stringify(data));
}

function handleInit(args) {
    // Stub for init
    outputJSON({ success: true, message: "init command stub" });
}

function handlePush(args) {
    // Stub for push
    outputJSON({ success: true, message: "push command stub" });
}

function handlePull(args) {
    // Stub for pull
    outputJSON({ success: true, message: "pull command stub" });
}

function handleLog(args) {
    // Stub for log
    outputJSON({ success: true, message: "log command stub" });
}

function handleStatus(args) {
    // Stub for status
    outputJSON({ success: true, message: "status command stub" });
}

switch (command) {
    case 'init':
        handleInit(args);
        break;
    case 'push':
        handlePush(args);
        break;
    case 'pull':
        handlePull(args);
        break;
    case 'log':
        handleLog(args);
        break;
    case 'status':
        handleStatus(args);
        break;
    default:
        outputJSON({ success: false, error: `Unknown command: ${command}` });
        break;
}
