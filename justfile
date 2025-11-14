test:
    watchexec -e rs,toml cargo test

cover:
    cargo llvm-cov --lcov --output-path lcov.info

changelog:
    npx standard-version --skip.bump --skip.commit --skip.tag --dry-run=false
