// Simple Node.js example for Sudachi WASM
// Usage: node example.js

const fs = require('fs');
const path = require('path');

// Note: For Node.js, you need to generate bindings with --target nodejs
// wasm-bindgen ../target/wasm32-unknown-unknown/release/sudachi_wasm.wasm --out-dir pkg-node --target nodejs

async function main() {
    try {
        // Import the WASM module (adjust path as needed)
        const { loadDictionary, tokenize, freeDictionary } = require('./pkg-node/sudachi_wasm.js');
        
        console.log('Loading dictionary...');
        
        // Read dictionary file
        const dictPath = path.join(__dirname, '../../resources/system.xdic');
        const dictBytes = new Uint8Array(fs.readFileSync(dictPath));
        
        // Load dictionary
        const handle = loadDictionary(dictBytes);
        console.log(`Dictionary loaded! Handle: ${handle}\n`);
        
        // Test texts
        const testCases = [
            { text: '選挙管理委員会', mode: 2, modeName: 'C (long)' },
            { text: '選挙管理委員会', mode: 0, modeName: 'A (short)' },
            { text: '東京スカイツリー', mode: 2, modeName: 'C (long)' },
            { text: '高輪ゲートウェイ駅', mode: 2, modeName: 'C (long)' },
        ];
        
        for (const testCase of testCases) {
            console.log(`Input: ${testCase.text} (Mode ${testCase.modeName})`);
            console.log('-'.repeat(60));
            
            const tokens = tokenize(handle, testCase.text, testCase.mode);
            
            tokens.forEach(token => {
                console.log(`${token.surface}\t${token.reading}\t${token.pos}`);
            });
            
            console.log('');
        }
        
        // Clean up
        freeDictionary(handle);
        console.log('Dictionary freed.');
        
    } catch (error) {
        console.error('Error:', error.message);
        console.error('\nMake sure to:');
        console.error('1. Generate Node.js bindings: wasm-bindgen ... --target nodejs');
        console.error('2. Have the dictionary file available');
    }
}

main();
