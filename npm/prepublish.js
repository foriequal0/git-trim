const fs = require("fs");
const path = require("path");

const toml = require('toml');
const cargoToml = toml.parse(fs.readFileSync(path.join(__dirname, "../Cargo.toml"), "utf-8"));
const packageJson = require("../package.json");
packageJson.version = cargoToml.package.version;
packageJson.description = cargoToml.package.description;
packageJson.keywords = cargoToml.package.keywords;
packageJson.license = cargoToml.package.license;
packageJson.author = cargoToml.package.authors[0];
packageJson.repository = {
  type: "git",
  url: cargoToml.package.repository,
};

fs.writeFileSync(path.join(__dirname, "../package.json"), JSON.stringify(packageJson, null, 2));
