// Test the webtts.js SDK in Node.js to observe the WASM init protocol
const fs = require('fs');
const path = require('path');

// Create metadata file
const voiceDir = 'wasm/voicedata_enu';
const files = fs.readdirSync(voiceDir)
  .filter(f => fs.statSync(path.join(voiceDir, f)).isFile())
  .map(f => ({
    name: f,
    size: fs.statSync(path.join(voiceDir, f)).size,
    url: f,
    md5: ''
  }));
const metadataPath = path.join(voiceDir, 'files.metadata');
fs.writeFileSync(metadataPath, JSON.stringify({files}));

// Load the SDK - it expects to run in a browser-like env
// Patch globalThis for Node.js compatibility
globalThis.alert = (msg) => console.log('[ALERT]', msg);
globalThis.location = { href: 'file:///test' };

// Load webtts.js which sets up the Module
process.chdir('wasm');
try {
  require('./webtts.js');
  console.log('webtts.js loaded');
  console.log('ttsInitialize available:', typeof ttsInitialize);
} catch(e) {
  console.log('Load error:', e.message);
}

// Try to initialize
setTimeout(async () => {
  try {
    console.log('Calling ttsInitialize...');
    const result = await ttsInitialize({
      env: 'node',
      data: 'local',
      cache: 'none',
      metadata: fs.readFileSync(path.join('../', metadataPath), 'utf8'),
      localroot: path.join('../', voiceDir)
    }, (msg) => {
      console.log('[PROGRESS]', JSON.stringify(msg).substring(0, 100));
    });
    console.log('Init result:', result);

    // Try speak
    const speakResult = await ttsSpeak('Hello world', (msg) => {
      if (msg.type === 'audio') {
        console.log('[AUDIO] buffer length:', msg.buffer ? msg.buffer.length : 0);
      } else {
        console.log('[SPEAK]', JSON.stringify(msg).substring(0, 100));
      }
    });
    console.log('Speak result:', speakResult);
  } catch(e) {
    console.log('Error:', e.message || e);
  }
}, 2000);
