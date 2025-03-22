# just
Java build system written in Rust

## Disclaimer

- ⚠️ This project is an experiment and does not cover all capabilities of the popular Java build systems like Maven or Gradle
- ⚠️ Please, don't use it with the production code

## Capabilities
- ❌ compiling project into a single `*.jar` file
- ❌ compiling project consisting of multiple `*.java` files
- ❌ downloading dependencies provided in the `*.toml` file
- ❌ resolving depndendencies available in maven central repository
- ❌ resolving transitive dependencies
- ❌ running compiled project
- ❌ executing unit tests
- ❌ creating a "fat jar" with all dependencies included within it
- ❌ parallel execution to make build process faster
