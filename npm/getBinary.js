const { Binary } = require('binary-install');
const os = require('os');

function getPlatform() {
  const type = os.type();
  const arch = os.arch();

  if (type === 'Windows_NT' && arch === 'x64') return 'win';
  if (type === 'Linux' && arch === 'x64') return 'linux';
  if (type === 'Darwin' && arch === 'x64') return 'mac';

  throw new Error(`Unsupported platform: ${type} ${arch}`);
}

function getBinary() {
  const platform = getPlatform();
  const version = require('../package.json').version;
  const name = 'git-trim';
  const url = `https://github.com/foriequal0/${name}/releases/download/v${version}/git-trim-${platform}-v${version}.tgz`;
  return new Binary(url, { name });
}

module.exports = getBinary;
