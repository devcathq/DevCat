#!/bin/sh
# Rebuild devcat-jdbc.jar from DevcatJdbc.java. Needs a JDK 17+ (javac, jar); the jar runs on Java 11+.
# DevCat embeds the committed jar (src-tauri/src/db/jdbc.rs), so building DevCat needs no Java:
# run this only after changing the Java source, and commit the jar with it.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
bin=${JAVA_HOME:+$JAVA_HOME/bin/}
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
"${bin}javac" --release 11 -Xlint:-options -encoding UTF-8 -d "$out/classes" "$here/DevcatJdbc.java"
# a fixed date keeps the jar the same byte for byte when the source didn't change
# Add-Opens lets the helper set Java's default charset for drivers that use it (DevcatJdbc.defaultCharset)
printf 'Add-Opens: java.base/java.nio.charset\n' > "$out/manifest.txt"
"${bin}jar" --create --date=2000-01-01T00:00:02Z --file "$here/devcat-jdbc.jar" --manifest "$out/manifest.txt" --main-class DevcatJdbc -C "$out/classes" .
ls -l "$here/devcat-jdbc.jar"
