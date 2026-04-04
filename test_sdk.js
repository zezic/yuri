const fs = require('fs');
const path = require('path');

globalThis.alert = (msg) => console.log('[ALERT]', msg);
globalThis.location = {
  href: 'https://codefactoryglobal.com/webassembly/demo/',
  hostname: 'codefactoryglobal.com',
  protocol: 'https:',
  origin: 'https://codefactoryglobal.com'
};
globalThis.self = globalThis;
globalThis.navigator = { userAgent: 'Mozilla/5.0 Chrome/120' };

const voiceDir = path.resolve('wasm/voicedata_enu');

// Create metadata
const files = fs.readdirSync(voiceDir)
  .filter(f => fs.statSync(path.join(voiceDir, f)).isFile() && !f.endsWith('.metadata'))
  .map(f => ({name: f, size: fs.statSync(path.join(voiceDir, f)).size, url: f, md5: ''}));
fs.writeFileSync(path.join(voiceDir, 'files.metadata'), JSON.stringify({files}));

const src = fs.readFileSync(path.resolve('wasm/webtts.js'), 'utf8');
// Make require/process available inside the eval scope
const fn = new Function('require', 'process', '__dirname', '__filename',
  src + '; return {ttsInitialize, ttsSpeak, ttsGetVoiceList, ttsSetCurrentVoice, ttsGetState, z_d};');
const sdk = fn(require, process, path.resolve('wasm'), path.resolve('wasm/webtts.js'));

console.log('SDK loaded. State:', sdk.ttsGetState());

(async () => {
  try {
    console.log('\n=== INITIALIZING ===');
    const initResult = await sdk.ttsInitialize({
      env: 'node',
      data: 'local',
      cache: 'none',
      metadata: JSON.stringify({files}),
      localroot: voiceDir,
      licensingmode: 'unmetered',
      licensegraceperiod: 999999
    }, (msg) => {
      if (msg.type === 'download' || msg.type === 'runtime') {
        console.log('[INIT]', msg.type, msg.name || '', Math.round((msg.totalFilesProgress||0)*100)+'%');
      }
    });
    console.log('Init result:', initResult);
    console.log('State:', sdk.ttsGetState());
    console.log('Voices:', JSON.stringify(sdk.ttsGetVoiceList()));

    const voices = sdk.ttsGetVoiceList();
    console.log('Voices:', voices.length);

    // Instrument fs to find when voice file is opened
    const origOpen = fs.openSync;
    const origRead = fs.readSync;
    let fileOps = [];
    fs.openSync = function(p, flags) {
      const fd = origOpen.apply(this, arguments);
      if (typeof p === 'string' && p.includes('voicedata')) {
        console.log('[FS.OPEN]', p, '→ fd=' + fd);
      }
      return fd;
    };
    fs.readSync = function(fd, buf, off, len, pos) {
      fileOps.push('read:fd='+fd+',len='+len);
      return origRead.apply(this, arguments);
    };

    console.log('\n=== SET VOICE ===');
    await sdk.ttsSetCurrentVoice(voices[0]);
    console.log('Voice set. File ops:', fileOps.length);

    console.log('\n=== SPEAKING ===');
    let audioChunks = 0;
    let totalSamples = 0;
    const allSamples = [];
    const speakResult = await sdk.ttsSpeak('Hello world', (msg) => {
      console.log('[SPEAK CB] keys:', Object.keys(msg).join(','),
        msg.type ? 'type='+msg.type : '',
        msg.buffer ? 'buf='+msg.buffer.length : '',
        msg.completeCode !== undefined ? 'cc='+msg.completeCode : '');
      if (msg.buffer) {
        audioChunks++;
        totalSamples += msg.buffer.length;
        allSamples.push(...Array.from(msg.buffer));
        if (audioChunks <= 3) {
          console.log('[AUDIO] first8:', Array.from(msg.buffer.slice(0,8)));
        }
      }
    });
    console.log('Speak result:', speakResult, 'Chunks:', audioChunks, 'Total:', totalSamples);
    console.log('File ops during speak:', fileOps.length);
    fileOps.forEach(op => console.log('  ', op));
    if (totalSamples > 0) {
      // Convert Float32 samples to Int16 WAV
      const sampleRate = 22050;
      const numSamples = totalSamples;
      const dataSize = numSamples * 2;
      const header = Buffer.alloc(44);
      // RIFF header
      header.write('RIFF', 0);
      header.writeUInt32LE(36 + dataSize, 4);
      header.write('WAVE', 8);
      header.write('fmt ', 12);
      header.writeUInt32LE(16, 16);
      header.writeUInt16LE(1, 20); // PCM
      header.writeUInt16LE(1, 22); // mono
      header.writeUInt32LE(sampleRate, 24);
      header.writeUInt32LE(sampleRate * 2, 28); // byte rate
      header.writeUInt16LE(2, 32); // block align
      header.writeUInt16LE(16, 34); // bits per sample
      header.write('data', 36);
      header.writeUInt32LE(dataSize, 40);

      const pcm = Buffer.alloc(dataSize);
      allSamples.forEach((v, i) => {
        const s = Math.max(-1, Math.min(1, v));
        pcm.writeInt16LE(Math.round(s * 32767), i * 2);
      });
      fs.writeFileSync('/tmp/node_hello.wav', Buffer.concat([header, pcm]));
      console.log('WAV saved: /tmp/node_hello.wav (' + numSamples + ' samples, ' + (numSamples/sampleRate).toFixed(2) + 's)');
    }
  } catch(e) {
    console.log('ERROR:', e.message || e);
    console.error(e.stack);
  }
})();
