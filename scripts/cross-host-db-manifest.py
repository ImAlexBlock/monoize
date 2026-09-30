"""Compute a read-only database manifest without exporting business rows."""

import argparse
import base64
import hashlib
import json
import os
import pathlib
import subprocess
import tempfile
import threading
from datetime import datetime, timezone


def identifier(value):
    if any(character in value for character in "\r\n\x00"):
        raise ValueError("Unsupported control character in SQL identifier")
    return '"' + value.replace('"', '""') + '"'


def table_query(schema, table):
    relation = identifier(schema) + "." + identifier(table)
    return (
        f"SELECT to_jsonb(t)::text FROM {relation} AS t "
        'ORDER BY to_jsonb(t)::text COLLATE "C"'
    )


def consume(stream, labels):
    results = {}
    digest = None
    label = None
    count = 0
    expected = iter(labels)
    for line in stream:
        if line.startswith(b"CHV_BEGIN "):
            if label is not None:
                raise RuntimeError("Nested manifest section")
            label = base64.b64decode(line.split()[1], validate=True).decode("utf-8")
            if label != next(expected, None):
                raise RuntimeError("Unexpected manifest section")
            digest = hashlib.sha256()
            count = 0
        elif line == b"CHV_END\n":
            if label is None:
                raise RuntimeError("Section ended before it started")
            results[label] = {"rows": count, "sha256": digest.hexdigest()}
            label = None
        else:
            if label is None:
                raise RuntimeError("Unexpected output outside manifest section")
            digest.update(line)
            count += 1
    if label is not None or next(expected, None) is not None:
        raise RuntimeError("Incomplete database manifest")
    return results


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--container", required=True)
    parser.add_argument("--user", required=True)
    parser.add_argument("--database", required=True)
    parser.add_argument("--port", default="5432")
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    output = pathlib.Path(args.output)
    if output.exists():
        raise SystemExit("Refusing to replace an existing manifest")
    base = [
        "docker", "exec", "-i", args.container, "psql", "-X", "-q", "-A", "-t",
        "-v", "ON_ERROR_STOP=1", "-U", args.user, "-p", args.port, "-d", args.database,
    ]
    catalog = (
        "SELECT json_build_array(schemaname,tablename)::text FROM pg_tables "
        "WHERE schemaname NOT IN ('pg_catalog','information_schema') "
        "AND schemaname NOT LIKE 'pg_toast%' ORDER BY schemaname,tablename"
    )
    tables = [
        json.loads(line)
        for line in subprocess.check_output(base + ["-c", catalog], text=True).splitlines()
    ]
    sections = [
        ("table:" + schema + "." + name, table_query(schema, name))
        for schema, name in tables
    ]
    metadata = {
        "columns": "SELECT table_schema,table_name,column_name,ordinal_position,column_default,is_nullable,data_type,udt_name,character_maximum_length,numeric_precision,numeric_scale FROM information_schema.columns WHERE table_schema NOT IN ('pg_catalog','information_schema')",
        "constraints": "SELECT n.nspname AS schema,c.relname AS table_name,k.conname,k.contype,k.convalidated,pg_get_constraintdef(k.oid,true) AS definition FROM pg_constraint k JOIN pg_class c ON c.oid=k.conrelid JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname NOT IN ('pg_catalog','information_schema')",
        "indexes": "SELECT schemaname,tablename,indexname,indexdef FROM pg_indexes WHERE schemaname NOT IN ('pg_catalog','information_schema')",
        "extensions": "SELECT extname,extversion FROM pg_extension",
        "sequences": "SELECT schemaname,sequencename,data_type,start_value,min_value,max_value,increment_by,cycle,cache_size,last_value FROM pg_sequences WHERE schemaname NOT IN ('pg_catalog','information_schema')",
    }
    for name, query in metadata.items():
        sections.append((
            "metadata:" + name,
            f'SELECT to_jsonb(t)::text FROM ({query}) t ORDER BY to_jsonb(t)::text COLLATE "C"',
        ))
    sequence_catalog = "SELECT json_build_array(schemaname,sequencename)::text FROM pg_sequences WHERE schemaname NOT IN ('pg_catalog','information_schema') ORDER BY schemaname,sequencename"
    for line in subprocess.check_output(base + ["-c", sequence_catalog], text=True).splitlines():
        schema, name = json.loads(line)
        relation = identifier(schema) + "." + identifier(name)
        sections.append((
            "sequence:" + schema + "." + name,
            f"SELECT to_jsonb(t)::text FROM (SELECT last_value,is_called FROM {relation}) t",
        ))
    commands = [
        "BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY;",
        "SET LOCAL timezone='UTC';",
        "SET LOCAL extra_float_digits=3;",
        "SET LOCAL statement_timeout='300s';",
        "SET LOCAL lock_timeout='5s';",
    ]
    for label, query in sections:
        marker = base64.b64encode(label.encode()).decode()
        commands.extend([
            "\\echo CHV_BEGIN " + marker,
            "\\copy (" + query + ") TO STDOUT WITH (FORMAT text)",
            "\\echo CHV_END",
        ])
    commands.append("COMMIT;")
    process = subprocess.Popen(base, stdin=subprocess.PIPE, stdout=subprocess.PIPE)
    writer_errors = []

    def send():
        try:
            with process.stdin:
                process.stdin.write(("\n".join(commands) + "\n").encode())
        except Exception as error:
            writer_errors.append(error)

    writer = threading.Thread(target=send)
    writer.start()
    try:
        results = consume(process.stdout, [label for label, _ in sections])
        writer.join()
        if process.wait() != 0 or writer_errors:
            raise RuntimeError("Database manifest query failed")
    except BaseException:
        process.kill()
        process.wait()
        writer.join()
        raise
    report = {
        "format": 1,
        "captured_at": datetime.now(timezone.utc).isoformat(),
        "container": args.container,
        "verification": results,
    }
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w", encoding="utf-8", dir=output.parent, delete=False,
        ) as destination:
            temporary = pathlib.Path(destination.name)
            json.dump(report, destination, sort_keys=True, indent=2)
            destination.flush()
            os.fsync(destination.fileno())
        os.link(temporary, output)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)
    print(json.dumps({"manifest": str(output), "sections": len(results)}))


if __name__ == "__main__":
    main()
