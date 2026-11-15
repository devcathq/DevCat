// DevCat's JDBC bridge: a small helper that DevCat (Rust) starts with `java -jar devcat-jdbc.jar`,
// on this computer or on another one over SSH, to reach a database through a JDBC driver jar the
// user supplies. It speaks one JSON object per line: requests on stdin, answers on stdout.
//
// - No dependencies beyond the JDK (Java 11+): the JSON reader / writer below is our own.
// - One process holds one JDBC connection. DevCat starts another for a read-only session.
// - Requests run one at a time on a worker thread, except `cancel`, which the reader thread handles
//   at once (it cancels the statement that's running).
// - stdout is the protocol only: System.out is pointed at stderr, so a driver that prints can't
//   break it.
//
// Build: src-tauri/jdbc-bridge/build.sh (javac --release 11), which writes devcat-jdbc.jar next to
// this file; DevCat embeds that jar (`db/jdbc.rs`).

import java.io.*;
import java.math.BigDecimal;
import java.math.BigInteger;
import java.net.URL;
import java.net.URLClassLoader;
import java.nio.charset.Charset;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.sql.*;
import java.util.*;
import java.util.concurrent.*;

public final class DevcatJdbc {
    static final int PROTOCOL = 2;
    static final int MAX_BINARY = 4096;
    static final int MAX_TEXT = 1_000_000;
    static final int MAX_RESULTS = 100;

    static PrintStream out;
    static Connection conn;
    static boolean readOnly;
    static volatile Statement running;
    // what the worker is doing, for the watchdog: a request stuck in a driver shows in the log
    static volatile String doing;
    static volatile long since;
    static volatile Thread workerThread;
    static final long SLOW_MS = 20_000;

    public static void main(String[] args) throws Exception {
        out = new PrintStream(new FileOutputStream(FileDescriptor.out), false, "UTF-8");
        System.setOut(System.err);
        BufferedReader in = new BufferedReader(new InputStreamReader(System.in, StandardCharsets.UTF_8));
        ExecutorService worker = Executors.newSingleThreadExecutor(r -> {
            Thread t = new Thread(r, "devcat-jdbc-worker");
            t.setDaemon(true);
            workerThread = t;
            return t;
        });
        watchdog();
        Map<String, Object> ready = new LinkedHashMap<>();
        ready.put("ready", true);
        ready.put("protocol", PROTOCOL);
        ready.put("java", System.getProperty("java.version"));
        send(ready);
        String line;
        while ((line = in.readLine()) != null) {
            if (line.isBlank()) continue;
            final Map<String, Object> req;
            try {
                req = Json.obj(Json.parse(line));
            } catch (Exception e) {
                answer(null, null, "bad request: " + e.getMessage());
                continue;
            }
            Object id = req.get("id");
            String op = str(req, "op");
            if ("cancel".equals(op)) {
                Statement s = running;
                try {
                    if (s != null) s.cancel();
                    answer(id, s != null, null);
                } catch (Exception e) {
                    answer(id, null, message(e));
                }
                continue;
            }
            if ("close".equals(op)) break;
            worker.submit(() -> {
                String sql = str(req, "sql");
                doing = op + (sql == null ? "" : ": " + (sql.length() > 500 ? sql.substring(0, 500) + "…" : sql));
                since = System.currentTimeMillis();
                try {
                    answer(id, handle(op, req), null);
                } catch (Throwable e) {
                    answer(id, null, message(e));
                } finally {
                    long took = System.currentTimeMillis() - since;
                    if (took >= SLOW_MS) log("finished after " + took / 1000 + " s: " + doing);
                    doing = null;
                }
            });
        }
        // stdin closed (DevCat went away) or `close`: let a running statement stop, then leave.
        Statement s = running;
        if (s != null) try { s.cancel(); } catch (Exception ignored) {}
        worker.shutdown();
        worker.awaitTermination(5, TimeUnit.SECONDS);
        if (conn != null) try { if (readOnly) conn.rollback(); conn.close(); } catch (Exception ignored) {}
        System.exit(0);
    }

    /**
     * Every few seconds: a request running for SLOW_MS goes to stderr (DevCat keeps it in
     * <data>/jdbc/helper.log) with where the worker is, then again each minute. A driver that
     * hangs (waiting on the server, a lock, a loop) shows where.
     */
    static void watchdog() {
        Thread w = new Thread(() -> {
            long told = 0;
            String toldFor = null;
            while (true) {
                try { Thread.sleep(5000); } catch (InterruptedException e) { return; }
                String d = doing;
                if (d == null) { toldFor = null; continue; }
                long took = System.currentTimeMillis() - since;
                if (took < SLOW_MS) continue;
                if (d.equals(toldFor) && took - told < 60_000) continue;
                toldFor = d;
                told = took;
                StringBuilder b = new StringBuilder("still running after " + took / 1000 + " s: " + d);
                Thread t = workerThread;
                if (t != null) for (StackTraceElement e : t.getStackTrace()) b.append("\n    at ").append(e);
                log(b.toString());
            }
        }, "devcat-jdbc-watchdog");
        w.setDaemon(true);
        w.start();
    }

    static void log(String m) {
        System.err.println("[devcat-jdbc " + ProcessHandle.current().pid() + " " + java.time.LocalDateTime.now().withNano(0) + "] " + m);
        System.err.flush();
    }

    static synchronized void send(Map<String, Object> m) {
        out.print(Json.write(m));
        out.print('\n');
        out.flush();
    }

    static void answer(Object id, Object result, String error) {
        Map<String, Object> m = new LinkedHashMap<>();
        m.put("id", id);
        if (error != null) {
            m.put("ok", false);
            m.put("error", error);
        } else {
            m.put("ok", true);
            m.put("result", result);
        }
        send(m);
    }

    static String message(Throwable e) {
        StringBuilder b = new StringBuilder();
        Throwable t = e;
        Set<Throwable> seen = new HashSet<>();
        while (t != null && seen.add(t)) {
            String m = t.getMessage();
            if (m == null || m.isBlank()) m = t.getClass().getSimpleName();
            if (t instanceof SQLException) {
                SQLException q = (SQLException) t;
                if (q.getSQLState() != null) m = m + " [SQLState " + q.getSQLState() + (q.getErrorCode() != 0 ? ", code " + q.getErrorCode() : "") + "]";
            }
            if (b.indexOf(m) < 0) {
                if (b.length() > 0) b.append(": ");
                b.append(m);
            }
            t = t instanceof SQLException && ((SQLException) t).getNextException() != null ? ((SQLException) t).getNextException() : t.getCause();
        }
        return b.toString();
    }

    // ---------------------------------------------------------------------------------------------

    static Object handle(String op, Map<String, Object> req) throws Exception {
        switch (op == null ? "" : op) {
            case "connect": return connect(req);
            case "ping": return ping();
            case "exec": return exec(req);
            case "batch": return batch(req);
            case "catalogs": return catalogs();
            case "schemas": return schemas(str(req, "catalog"));
            case "tables": return tables(req);
            case "columns": return columns(str(req, "catalog"), str(req, "schema"), str(req, "table"));
            case "structure": return structure(str(req, "catalog"), str(req, "schema"), str(req, "table"));
            case "procedures": return procedures(str(req, "catalog"), str(req, "schema"));
            case "snapshot": return snapshot(req);
            default: throw new IllegalArgumentException("unknown op " + op);
        }
    }

    // isValid first; but some drivers answer false whatever the state (SQLBase's isValid is a stub that
    // returns false) or don't have it, so before calling the connection lost, ask the server something real
    static boolean ping() {
        Connection c = need();
        try {
            if (c.isValid(10)) return true;
        } catch (SQLException | RuntimeException | AbstractMethodError ignored) {}
        try {
            if (c.isClosed()) return false;
            try (ResultSet rs = c.getMetaData().getTables(null, null, "DEVCAT_PING_", null)) {
                rs.next();
            }
            return true;
        } catch (SQLException | RuntimeException e) {
            return false;
        }
    }

    static Connection need() {
        if (conn == null) throw new IllegalStateException("not connected");
        return conn;
    }

    // Older drivers turn bytes into text with the JVM's default charset (new String(bytes), getBytes()),
    // and SQLBase's does so for column and table names, values and the SQL it sends. Java 18+ fixes that
    // default to UTF-8 and ignores -Dfile.encoding=MS932, so for a database in a legacy code page the
    // helper sets it itself, before the driver loads. The jar's manifest opens java.nio.charset for this.
    static void defaultCharset(String name) {
        Charset cs;
        try {
            cs = Charset.forName(name);
        } catch (IllegalArgumentException e) {
            throw new IllegalArgumentException("this Java doesn't know the text encoding " + name);
        }
        System.setProperty("file.encoding", cs.name());
        if (Charset.defaultCharset().equals(cs)) return;
        try {
            java.lang.reflect.Field f = Charset.class.getDeclaredField("defaultCharset");
            f.setAccessible(true);
            f.set(null, cs);
        } catch (ReflectiveOperationException | RuntimeException e) {
            throw new IllegalStateException("couldn't switch Java's text encoding to " + cs.name() + " (" + e + ")");
        }
        if (!Charset.defaultCharset().equals(cs)) throw new IllegalStateException("couldn't switch Java's text encoding to " + cs.name());
    }

    static Object connect(Map<String, Object> req) throws Exception {
        if (conn != null) throw new IllegalStateException("already connected");
        String enc = str(req, "encoding");
        if (enc != null && !enc.isBlank()) defaultCharset(enc.trim());
        List<URL> urls = new ArrayList<>();
        for (Object o : list(req, "jars")) {
            Path p = Paths.get(home(String.valueOf(o)));
            if (Files.isDirectory(p)) {
                try (DirectoryStream<Path> ds = Files.newDirectoryStream(p, "*.jar")) {
                    for (Path j : ds) urls.add(j.toUri().toURL());
                }
            } else if (Files.isRegularFile(p)) {
                urls.add(p.toUri().toURL());
            } else {
                throw new FileNotFoundException("driver jar not found: " + p);
            }
        }
        ClassLoader loader = new URLClassLoader(urls.toArray(new URL[0]), DevcatJdbc.class.getClassLoader());
        Thread.currentThread().setContextClassLoader(loader);
        String url = str(req, "url");
        if (url == null || url.isBlank()) throw new IllegalArgumentException("no JDBC URL");
        String cls = str(req, "driverClass");
        Driver driver = null;
        if (cls != null && !cls.isBlank()) {
            driver = (Driver) Class.forName(cls.trim(), true, loader).getDeclaredConstructor().newInstance();
        } else {
            for (Driver d : ServiceLoader.load(Driver.class, loader)) {
                try {
                    if (d.acceptsURL(url)) { driver = d; break; }
                } catch (SQLException ignored) {}
            }
            if (driver == null) driver = scanJars(urls, loader, url);
            if (driver == null) {
                // drivers already on the class path (or registered the old way)
                try { driver = DriverManager.getDriver(url); } catch (SQLException ignored) {}
            }
            if (driver == null) throw new IllegalArgumentException("no driver in the jars accepts this URL; check its jdbc:<name>: prefix, or set the driver class");
        }
        Properties props = new Properties();
        Map<String, Object> extra = obj(req, "props");
        for (Map.Entry<String, Object> e : extra.entrySet()) if (e.getValue() != null) props.setProperty(e.getKey(), String.valueOf(e.getValue()));
        String user = str(req, "user");
        String password = str(req, "password");
        if (user != null && !user.isEmpty()) props.setProperty("user", user);
        if (password != null && !password.isEmpty()) props.setProperty("password", password);
        DriverManager.setLoginTimeout(30);
        Connection c = driver.connect(url, props);
        if (c == null) throw new IllegalArgumentException("the driver " + driver.getClass().getName() + " doesn't accept this URL");
        readOnly = Boolean.TRUE.equals(req.get("readOnly"));
        if (readOnly) {
            // A hint many drivers ignore; every statement also runs in a transaction that is
            // rolled back afterwards (`finish`).
            try { c.setReadOnly(true); } catch (Exception ignored) {}
            c.setAutoCommit(false);
        }
        conn = c;
        DatabaseMetaData md = c.getMetaData();
        Map<String, Object> info = new LinkedHashMap<>();
        info.put("product", safe(md::getDatabaseProductName));
        info.put("productVersion", safe(md::getDatabaseProductVersion));
        info.put("driver", safe(md::getDriverName));
        info.put("driverVersion", safe(md::getDriverVersion));
        info.put("driverClass", driver.getClass().getName());
        info.put("quote", safe(md::getIdentifierQuoteString));
        info.put("catalogSeparator", safe(md::getCatalogSeparator));
        info.put("catalogTerm", safe(md::getCatalogTerm));
        info.put("schemaTerm", safe(md::getSchemaTerm));
        info.put("catalogs", bool(md::supportsCatalogsInDataManipulation) || bool(md::supportsCatalogsInTableDefinitions));
        info.put("schemas", bool(md::supportsSchemasInDataManipulation) || bool(md::supportsSchemasInTableDefinitions));
        info.put("catalog", safe(c::getCatalog));
        info.put("schema", safe(c::getSchema));
        info.put("user", safe(md::getUserName));
        List<Object> types = new ArrayList<>();
        try (ResultSet rs = md.getTableTypes()) {
            while (rs.next()) {
                String t = rs.getString(1);
                if (t != null && !t.isBlank()) types.add(t.trim());
            }
        } catch (Exception ignored) {}
        info.put("tableTypes", types);
        info.put("readOnly", readOnly);
        return info;
    }

    /**
     * Older drivers (SQLBase's, for one) don't list themselves for ServiceLoader: look in the jars
     * for a class named `…Driver` that is a java.sql.Driver and takes the URL.
     */
    static Driver scanJars(List<URL> urls, ClassLoader loader, String url) {
        int tried = 0;
        for (URL u : urls) {
            try (java.util.jar.JarFile jar = new java.util.jar.JarFile(new File(u.toURI()))) {
                for (Enumeration<java.util.jar.JarEntry> en = jar.entries(); en.hasMoreElements() && tried < 200; ) {
                    String name = en.nextElement().getName();
                    if (!name.endsWith("Driver.class") || name.contains("$") || name.startsWith("META-INF/")) continue;
                    tried++;
                    try {
                        Class<?> c = Class.forName(name.substring(0, name.length() - 6).replace('/', '.'), false, loader);
                        if (!Driver.class.isAssignableFrom(c) || c.isInterface() || java.lang.reflect.Modifier.isAbstract(c.getModifiers())) continue;
                        Driver d = (Driver) c.getDeclaredConstructor().newInstance();
                        if (d.acceptsURL(url)) return d;
                    } catch (Throwable ignored) {}
                }
            } catch (Exception ignored) {}
        }
        return null;
    }

    interface Get<T> { T get() throws Exception; }

    static <T> T safe(Get<T> g) {
        try { return g.get(); } catch (Throwable e) { return null; }
    }

    static boolean bool(Get<Boolean> g) {
        Boolean b = safe(g);
        return b != null && b;
    }

    /**
     * Point the session at `catalog` / `schema` when asked. Compared with what the connection
     * says now, never a remembered value: a query may have run `USE` / `SET SCHEMA` since.
     */
    static void context(Map<String, Object> req) throws SQLException {
        String cat = str(req, "catalog");
        if (cat != null && !cat.isEmpty() && !cat.equals(safe(conn::getCatalog))) conn.setCatalog(cat);
        String sch = str(req, "schema");
        if (sch != null && !sch.isEmpty() && !sch.equals(safe(conn::getSchema))) {
            try {
                conn.setSchema(sch);
            } catch (SQLException | AbstractMethodError ignored) {
                // can't switch (SQLBase's says "Unsupported method"); DevCat qualifies names with the schema
            }
        }
    }

    /** After a statement on a read-only session: undo whatever it did. */
    static void finish() {
        if (readOnly) try { conn.rollback(); } catch (Exception ignored) {}
    }

    static Object exec(Map<String, Object> req) throws Exception {
        Connection c = need();
        context(req);
        String sql = str(req, "sql");
        int maxRows = num(req, "maxRows", 1000);
        int skip = num(req, "skip", 0);
        List<Object> params = req.get("params") instanceof List ? list(req, "params") : null;
        List<Object> results = new ArrayList<>();
        Statement st = params != null ? c.prepareStatement(sql) : c.createStatement();
        running = st;
        try {
            try { st.setMaxRows(skip + maxRows + 1); } catch (Exception ignored) {}
            try { st.setFetchSize(Math.min(500, skip + maxRows + 1)); } catch (Exception ignored) {}
            boolean isRs;
            if (params != null) {
                bind((PreparedStatement) st, params);
                isRs = ((PreparedStatement) st).execute();
            } else {
                isRs = st.execute(sql);
            }
            for (int n = 0; n < MAX_RESULTS; n++) {
                if (isRs) {
                    try (ResultSet rs = st.getResultSet()) {
                        results.add(readRows(rs, skip, maxRows));
                    }
                } else {
                    long uc;
                    try { uc = st.getLargeUpdateCount(); } catch (Exception e) { uc = st.getUpdateCount(); }
                    if (uc == -1) break;
                    Map<String, Object> r = new LinkedHashMap<>();
                    r.put("updateCount", uc);
                    results.add(r);
                }
                try { isRs = st.getMoreResults(); } catch (SQLException e) { break; }
            }
            SQLWarning w = safe(st::getWarnings);
            if (w != null && !results.isEmpty()) {
                StringBuilder b = new StringBuilder();
                for (int i = 0; w != null && i < 20; i++, w = w.getNextWarning()) {
                    if (b.length() > 0) b.append('\n');
                    b.append(w.getMessage());
                }
                @SuppressWarnings("unchecked") Map<String, Object> last = (Map<String, Object>) results.get(results.size() - 1);
                last.put("message", b.toString());
            }
        } finally {
            running = null;
            try { st.close(); } catch (Exception ignored) {}
            finish();
        }
        return results;
    }

    /** Grid edits: every statement in one transaction, rolled back if any fails or touches the wrong number of rows. */
    static Object batch(Map<String, Object> req) throws Exception {
        Connection c = need();
        if (readOnly) throw new SQLException("this session is read-only");
        context(req);
        boolean auto = c.getAutoCommit();
        c.setAutoCommit(false);
        long total = 0;
        int i = 0;
        try {
            for (Object o : list(req, "statements")) {
                i++;
                Map<String, Object> s = Json.obj(o);
                try (PreparedStatement ps = c.prepareStatement(str(s, "sql"))) {
                    running = ps;
                    bind(ps, s.get("params") instanceof List ? list(s, "params") : Collections.emptyList());
                    long n = ps.executeUpdate();
                    if (Boolean.TRUE.equals(s.get("expectOne")) && n != 1) {
                        throw new SQLException("change " + i + " affected " + n + " rows (expected exactly 1); all changes were rolled back");
                    }
                    total += n;
                } finally {
                    running = null;
                }
            }
            c.commit();
        } catch (Exception e) {
            try { c.rollback(); } catch (Exception ignored) {}
            throw e;
        } finally {
            try { c.setAutoCommit(auto); } catch (Exception ignored) {}
        }
        return total;
    }

    static void bind(PreparedStatement ps, List<Object> params) throws SQLException {
        ParameterMetaData pm = safe(ps::getParameterMetaData);
        for (int i = 0; i < params.size(); i++) {
            Object v = params.get(i);
            int n = i + 1;
            if (v == null) {
                int t = Types.VARCHAR;
                if (pm != null) try { t = pm.getParameterType(n); } catch (Exception ignored) {}
                ps.setNull(n, t);
            } else if (v instanceof Boolean) {
                ps.setBoolean(n, (Boolean) v);
            } else if (v instanceof Long) {
                ps.setLong(n, (Long) v);
            } else if (v instanceof BigInteger) {
                ps.setBigDecimal(n, new BigDecimal((BigInteger) v));
            } else if (v instanceof BigDecimal) {
                ps.setBigDecimal(n, (BigDecimal) v);
            } else if (v instanceof String) {
                String s = (String) v;
                byte[] bin = s.startsWith("\\x") ? hexBytes(s.substring(2)) : null;
                if (bin != null && pm != null && isBinary(pm, n)) ps.setBytes(n, bin);
                else ps.setString(n, s);
            } else {
                ps.setString(n, Json.write(v));
            }
        }
    }

    static boolean isBinary(ParameterMetaData pm, int n) {
        try {
            int t = pm.getParameterType(n);
            return t == Types.BINARY || t == Types.VARBINARY || t == Types.LONGVARBINARY || t == Types.BLOB;
        } catch (Exception e) {
            return false;
        }
    }

    static byte[] hexBytes(String h) {
        if (h.length() % 2 != 0) return null;
        byte[] b = new byte[h.length() / 2];
        for (int i = 0; i < b.length; i++) {
            int hi = Character.digit(h.charAt(2 * i), 16), lo = Character.digit(h.charAt(2 * i + 1), 16);
            if (hi < 0 || lo < 0) return null;
            b[i] = (byte) (hi * 16 + lo);
        }
        return b;
    }

    static Map<String, Object> readRows(ResultSet rs, int skip, int maxRows) throws SQLException {
        ResultSetMetaData md = rs.getMetaData();
        int n = md.getColumnCount();
        List<Object> cols = new ArrayList<>();
        int[] types = new int[n];
        for (int i = 1; i <= n; i++) {
            final int k = i;
            Map<String, Object> col = new LinkedHashMap<>();
            String label = safe(() -> md.getColumnLabel(k));
            col.put("name", label != null && !label.isEmpty() ? label : md.getColumnName(i));
            col.put("type", safe(() -> md.getColumnTypeName(k)));
            types[i - 1] = md.getColumnType(i);
            cols.add(col);
        }
        List<Object> rows = new ArrayList<>();
        boolean truncated = false;
        int seen = 0;
        while (rs.next()) {
            if (seen++ < skip) continue;
            if (rows.size() >= maxRows) {
                truncated = true;
                break;
            }
            List<Object> row = new ArrayList<>(n);
            for (int i = 1; i <= n; i++) row.add(cell(rs, i, types[i - 1]));
            rows.add(row);
        }
        Map<String, Object> r = new LinkedHashMap<>();
        r.put("columns", cols);
        r.put("rows", rows);
        r.put("truncated", truncated);
        return r;
    }

    static Object cell(ResultSet rs, int i, int type) throws SQLException {
        switch (type) {
            case Types.BIT:
            case Types.BOOLEAN: {
                boolean b = rs.getBoolean(i);
                return rs.wasNull() ? null : b;
            }
            case Types.TINYINT:
            case Types.SMALLINT:
            case Types.INTEGER:
            case Types.BIGINT: {
                Object o = rs.getObject(i);
                if (o == null) return null;
                if (o instanceof BigInteger) return plain(new BigDecimal((BigInteger) o)); // BIGINT UNSIGNED
                if (o instanceof Number && !(o instanceof BigDecimal) && !(o instanceof Double) && !(o instanceof Float)) return ((Number) o).longValue();
                if (o instanceof BigDecimal) return plain((BigDecimal) o);
                return o.toString();
            }
            case Types.REAL:
            case Types.FLOAT:
            case Types.DOUBLE: {
                double d = rs.getDouble(i);
                if (rs.wasNull()) return null;
                return Double.isFinite(d) ? (Object) d : Double.toString(d);
            }
            case Types.NUMERIC:
            case Types.DECIMAL: {
                BigDecimal d = rs.getBigDecimal(i);
                return d == null ? null : d.toPlainString();
            }
            case Types.DATE: {
                java.sql.Date d = rs.getDate(i);
                return d == null ? null : d.toString();
            }
            case Types.TIME: {
                Time t = rs.getTime(i);
                return t == null ? null : t.toString();
            }
            case Types.TIMESTAMP:
            case Types.TIMESTAMP_WITH_TIMEZONE: {
                Object o;
                try { o = rs.getObject(i); } catch (SQLException e) { o = rs.getTimestamp(i); }
                if (o == null) return null;
                if (o instanceof Timestamp) {
                    String s = ((Timestamp) o).toString();
                    return s.endsWith(".0") ? s.substring(0, s.length() - 2) : s;
                }
                return o.toString().replace('T', ' ');
            }
            case Types.BINARY:
            case Types.VARBINARY:
            case Types.LONGVARBINARY:
            case Types.BLOB: {
                byte[] b;
                if (type == Types.BLOB) {
                    Blob blob = rs.getBlob(i);
                    if (blob == null) return null;
                    long len = blob.length();
                    b = blob.getBytes(1, (int) Math.min(len, MAX_BINARY + 1));
                } else {
                    b = rs.getBytes(i);
                }
                return b == null ? null : hex(b);
            }
            case Types.CLOB:
            case Types.NCLOB: {
                Clob c = type == Types.NCLOB ? rs.getNClob(i) : rs.getClob(i);
                if (c == null) return null;
                long len = c.length();
                String s = c.getSubString(1, (int) Math.min(len, MAX_TEXT));
                return len > MAX_TEXT ? s + "…" : s;
            }
            default: {
                Object o = rs.getObject(i);
                if (o == null) return null;
                if (o instanceof String) return text((String) o);
                if (o instanceof Boolean) return o;
                if (o instanceof BigDecimal) return ((BigDecimal) o).toPlainString();
                if (o instanceof Integer || o instanceof Long || o instanceof Short || o instanceof Byte) return ((Number) o).longValue();
                if (o instanceof byte[]) return hex((byte[]) o);
                if (o instanceof Clob) {
                    Clob c = (Clob) o;
                    long len = c.length();
                    return text(c.getSubString(1, (int) Math.min(len, MAX_TEXT + 1)));
                }
                if (o instanceof java.sql.Array) {
                    try {
                        Object a = ((java.sql.Array) o).getArray();
                        if (a instanceof Object[]) return text(Arrays.deepToString((Object[]) a));
                    } catch (Exception ignored) {}
                }
                return text(o.toString());
            }
        }
    }

    static Object plain(BigDecimal d) {
        try { return d.longValueExact(); } catch (ArithmeticException e) { return d.toPlainString(); }
    }

    static String text(String s) {
        return s.length() > MAX_TEXT ? s.substring(0, MAX_TEXT) + "…" : s;
    }

    static String hex(byte[] b) {
        boolean cut = b.length > MAX_BINARY;
        int n = cut ? MAX_BINARY : b.length;
        StringBuilder s = new StringBuilder(n * 2 + 3).append("\\x");
        for (int i = 0; i < n; i++) s.append(Character.forDigit((b[i] >> 4) & 15, 16)).append(Character.forDigit(b[i] & 15, 16));
        if (cut) s.append('…');
        return s.toString();
    }

    // ---------------------------------------------------------------------------------------------
    // Metadata

    /** `name` as a metadata search pattern that matches only itself (`_` and `%` escaped). */
    static String pattern(String name) {
        if (name == null) return null;
        String esc = safe(() -> need().getMetaData().getSearchStringEscape());
        if (esc == null || esc.isEmpty()) return name;
        return name.replace(esc, esc + esc).replace("_", esc + "_").replace("%", esc + "%");
    }

    /** A metadata row's catalog / schema is the one asked for (or none was asked for). */
    static boolean same(String asked, String got) {
        return asked == null || got == null || asked.equals(got);
    }

    static String nz(String s) {
        return s == null || s.isEmpty() ? null : s;
    }

    static Object catalogs() throws SQLException {
        List<Object> out = new ArrayList<>();
        try (ResultSet rs = need().getMetaData().getCatalogs()) {
            // SQLBase's lists one null catalog: a database without catalogs
            while (rs.next()) {
                String c = rs.getString(1);
                if (c != null && !c.isEmpty() && !out.contains(c)) out.add(c);
            }
        } finally {
            finish();
        }
        return out;
    }

    static Object schemas(String catalog) throws SQLException {
        List<Object> out = new ArrayList<>();
        DatabaseMetaData md = need().getMetaData();
        ResultSet rs;
        try {
            rs = md.getSchemas(nz(catalog), null);
        } catch (SQLException | AbstractMethodError e) {
            // JDBC 3 drivers (SQLBase's: a plain SQLException "Unsupported method") have only the old form
            rs = md.getSchemas();
        }
        try (ResultSet r = rs) {
            while (r.next()) {
                String cat = safe(() -> r.getString("TABLE_CATALOG"));
                if (catalog != null && !catalog.isEmpty() && cat != null && !cat.isEmpty() && !cat.equals(catalog)) continue;
                String s = r.getString(1);
                if (s != null && !out.contains(s)) out.add(s);
            }
        } finally {
            finish();
        }
        return out;
    }

    static Object tables(Map<String, Object> req) throws SQLException {
        List<Object> typesIn = req.get("types") instanceof List ? list(req, "types") : null;
        String[] types = typesIn == null ? null : typesIn.stream().map(String::valueOf).toArray(String[]::new);
        List<Object> out = new ArrayList<>();
        String cat = nz(str(req, "catalog")), sch = nz(str(req, "schema"));
        try (ResultSet rs = need().getMetaData().getTables(cat, pattern(sch), "%", types)) {
            while (rs.next()) {
                if (!same(sch, rs.getString("TABLE_SCHEM")) || !same(cat, rs.getString("TABLE_CAT"))) continue; // `_` is a pattern
                Map<String, Object> t = new LinkedHashMap<>();
                t.put("catalog", rs.getString("TABLE_CAT"));
                t.put("schema", rs.getString("TABLE_SCHEM"));
                t.put("name", rs.getString("TABLE_NAME"));
                t.put("type", rs.getString("TABLE_TYPE"));
                t.put("remarks", safe(() -> rs.getString("REMARKS")));
                out.add(t);
            }
        } finally {
            finish();
        }
        return out;
    }

    static List<Object> columns(String catalog, String schema, String table) throws SQLException {
        List<Object> out = new ArrayList<>();
        try (ResultSet rs = need().getMetaData().getColumns(nz(catalog), pattern(nz(schema)), pattern(table), "%")) {
            while (rs.next()) {
                String t = rs.getString("TABLE_NAME");
                if (table != null && t != null && !t.equals(table)) continue; // `_` in the name is a pattern
                if (!same(nz(schema), rs.getString("TABLE_SCHEM")) || !same(nz(catalog), rs.getString("TABLE_CAT"))) continue;
                out.add(column(rs));
            }
        } finally {
            finish();
        }
        return out;
    }

    static Map<String, Object> column(ResultSet rs) throws SQLException {
        Map<String, Object> c = new LinkedHashMap<>();
        c.put("table", rs.getString("TABLE_NAME"));
        c.put("name", rs.getString("COLUMN_NAME"));
        c.put("dataType", rs.getInt("DATA_TYPE"));
        c.put("typeName", rs.getString("TYPE_NAME"));
        int size = rs.getInt("COLUMN_SIZE");
        c.put("size", rs.wasNull() ? null : size);
        int digits = rs.getInt("DECIMAL_DIGITS");
        c.put("digits", rs.wasNull() ? null : digits);
        c.put("nullable", rs.getInt("NULLABLE") != DatabaseMetaData.columnNoNulls);
        c.put("default", safe(() -> rs.getString("COLUMN_DEF")));
        c.put("remarks", safe(() -> rs.getString("REMARKS")));
        c.put("ordinal", rs.getInt("ORDINAL_POSITION"));
        String auto = safe(() -> rs.getString("IS_AUTOINCREMENT"));
        c.put("autoIncrement", "YES".equalsIgnoreCase(auto));
        return c;
    }

    /** Columns, primary key, indexes and foreign keys of one table. */
    static Object structure(String catalog, String schema, String table) throws SQLException {
        Map<String, Object> s = new LinkedHashMap<>();
        s.put("name", table);
        s.put("columns", columns(catalog, schema, table));
        s.put("primaryKey", primaryKey(catalog, schema, table));
        s.put("indexes", indexes(catalog, schema, table));
        s.put("foreignKeys", foreignKeys(catalog, schema, table));
        return s;
    }

    static Object primaryKey(String catalog, String schema, String table) {
        TreeMap<Integer, Object> cols = new TreeMap<>();
        String[] name = {null};
        try (ResultSet rs = need().getMetaData().getPrimaryKeys(nz(catalog), nz(schema), table)) {
            while (rs.next()) {
                cols.put(rs.getInt("KEY_SEQ"), rs.getString("COLUMN_NAME"));
                name[0] = safe(() -> rs.getString("PK_NAME"));
            }
        } catch (Exception ignored) {
        } finally {
            finish();
        }
        Map<String, Object> pk = new LinkedHashMap<>();
        pk.put("name", name[0]);
        pk.put("columns", new ArrayList<>(cols.values()));
        return pk;
    }

    static Object indexes(String catalog, String schema, String table) {
        Map<String, Map<String, Object>> byName = new LinkedHashMap<>();
        Map<String, TreeMap<Integer, Object>> cols = new HashMap<>();
        try (ResultSet rs = need().getMetaData().getIndexInfo(nz(catalog), nz(schema), table, false, true)) {
            while (rs.next()) {
                if (rs.getShort("TYPE") == DatabaseMetaData.tableIndexStatistic) continue;
                String name = rs.getString("INDEX_NAME");
                String col = rs.getString("COLUMN_NAME");
                if (name == null || col == null) continue;
                boolean unique = !rs.getBoolean("NON_UNIQUE");
                Map<String, Object> ix = byName.computeIfAbsent(name, k -> {
                    Map<String, Object> m = new LinkedHashMap<>();
                    m.put("name", k);
                    m.put("unique", unique);
                    return m;
                });
                cols.computeIfAbsent(name, k -> new TreeMap<>()).put((int) rs.getShort("ORDINAL_POSITION"), col);
                ix.put("unique", unique);
            }
        } catch (Exception ignored) {
        } finally {
            finish();
        }
        List<Object> out = new ArrayList<>();
        for (Map.Entry<String, Map<String, Object>> e : byName.entrySet()) {
            e.getValue().put("columns", new ArrayList<>(cols.get(e.getKey()).values()));
            out.add(e.getValue());
        }
        return out;
    }

    static Object foreignKeys(String catalog, String schema, String table) {
        Map<String, Map<String, Object>> byName = new LinkedHashMap<>();
        try (ResultSet rs = need().getMetaData().getImportedKeys(nz(catalog), nz(schema), table)) {
            while (rs.next()) {
                String name = rs.getString("FK_NAME");
                String refTable = rs.getString("PKTABLE_NAME");
                if (name == null || name.isEmpty()) name = "fk_" + table + "_" + refTable;
                String refSchema = rs.getString("PKTABLE_SCHEM");
                short upd = rs.getShort("UPDATE_RULE"), del = rs.getShort("DELETE_RULE");
                Map<String, Object> fk = byName.computeIfAbsent(name, k -> {
                    Map<String, Object> m = new LinkedHashMap<>();
                    m.put("name", k);
                    m.put("refSchema", refSchema);
                    m.put("refTable", refTable);
                    m.put("columns", new ArrayList<>());
                    m.put("refColumns", new ArrayList<>());
                    m.put("onUpdate", rule(upd));
                    m.put("onDelete", rule(del));
                    return m;
                });
                ((List<Object>) fk.get("columns")).add(rs.getString("FKCOLUMN_NAME"));
                ((List<Object>) fk.get("refColumns")).add(rs.getString("PKCOLUMN_NAME"));
            }
        } catch (Exception ignored) {
        } finally {
            finish();
        }
        return new ArrayList<>(byName.values());
    }

    static String rule(short r) {
        switch (r) {
            case DatabaseMetaData.importedKeyCascade: return "CASCADE";
            case DatabaseMetaData.importedKeySetNull: return "SET NULL";
            case DatabaseMetaData.importedKeySetDefault: return "SET DEFAULT";
            case DatabaseMetaData.importedKeyRestrict: return "RESTRICT";
            default: return null;
        }
    }

    static Object procedures(String catalog, String schema) throws SQLException {
        List<Object> out = new ArrayList<>();
        try (ResultSet rs = need().getMetaData().getProcedures(nz(catalog), nz(schema), "%")) {
            while (rs.next()) {
                Map<String, Object> p = new LinkedHashMap<>();
                p.put("name", rs.getString("PROCEDURE_NAME"));
                p.put("remarks", safe(() -> rs.getString("REMARKS")));
                out.add(p);
            }
        } finally {
            finish();
        }
        return out;
    }

    /** Every table of a catalog / schema with its structure (ER diagram, agents' schema), up to `limit` tables. */
    static Object snapshot(Map<String, Object> req) throws SQLException {
        String catalog = str(req, "catalog"), schema = str(req, "schema");
        int limit = num(req, "limit", 500);
        @SuppressWarnings("unchecked") List<Object> tables = (List<Object>) tables(req);
        // columns for all tables at once: one metadata call instead of one per table
        Map<String, List<Object>> colsBy = new HashMap<>();
        try (ResultSet rs = need().getMetaData().getColumns(nz(catalog), pattern(nz(schema)), "%", "%")) {
            while (rs.next()) if (same(nz(schema), rs.getString("TABLE_SCHEM"))) colsBy.computeIfAbsent(rs.getString("TABLE_SCHEM") + "\u0000" + rs.getString("TABLE_NAME"), k -> new ArrayList<>()).add(column(rs));
        } catch (Exception ignored) {
        } finally {
            finish();
        }
        List<Object> out = new ArrayList<>();
        boolean cut = false;
        for (Object o : tables) {
            if (out.size() >= limit) { cut = true; break; }
            Map<String, Object> t = Json.obj(o);
            String name = str(t, "name");
            String type = str(t, "type");
            Map<String, Object> s = new LinkedHashMap<>();
            s.put("name", name);
            s.put("schema", t.get("schema"));
            s.put("type", type);
            s.put("remarks", t.get("remarks"));
            List<Object> cols = colsBy.get(t.get("schema") + "\u0000" + name);
            s.put("columns", cols != null ? cols : columns(catalog, str(t, "schema"), name));
            boolean isTable = type == null || type.toUpperCase(Locale.ROOT).contains("TABLE");
            if (isTable) {
                s.put("primaryKey", primaryKey(catalog, str(t, "schema"), name));
                s.put("indexes", indexes(catalog, str(t, "schema"), name));
                s.put("foreignKeys", foreignKeys(catalog, str(t, "schema"), name));
            }
            out.add(s);
        }
        Map<String, Object> r = new LinkedHashMap<>();
        r.put("tables", out);
        r.put("truncated", cut);
        return r;
    }

    // ---------------------------------------------------------------------------------------------
    // Request helpers

    static String home(String p) {
        if (p.equals("~")) return System.getProperty("user.home");
        if (p.startsWith("~/") || p.startsWith("~\\")) return System.getProperty("user.home") + p.substring(1);
        return p;
    }

    static String str(Map<String, Object> m, String k) {
        Object v = m.get(k);
        return v == null ? null : String.valueOf(v);
    }

    static int num(Map<String, Object> m, String k, int dflt) {
        Object v = m.get(k);
        if (v instanceof Number) return (int) Math.max(0, Math.min(Integer.MAX_VALUE - 2, ((Number) v).longValue()));
        return dflt;
    }

    @SuppressWarnings("unchecked")
    static List<Object> list(Map<String, Object> m, String k) {
        Object v = m.get(k);
        return v instanceof List ? (List<Object>) v : Collections.emptyList();
    }

    static Map<String, Object> obj(Map<String, Object> m, String k) {
        Object v = m.get(k);
        return v instanceof Map ? Json.obj(v) : Collections.emptyMap();
    }

    // ---------------------------------------------------------------------------------------------

    /** Just enough JSON: objects, arrays, strings, numbers (Long, BigInteger, BigDecimal), booleans, null. */
    static final class Json {
        final String s;
        int i;

        Json(String s) { this.s = s; }

        @SuppressWarnings("unchecked")
        static Map<String, Object> obj(Object o) {
            if (!(o instanceof Map)) throw new IllegalArgumentException("expected an object");
            return (Map<String, Object>) o;
        }

        static Object parse(String s) {
            Json j = new Json(s);
            Object v = j.value();
            j.ws();
            if (j.i != s.length()) throw new IllegalArgumentException("trailing characters at " + j.i);
            return v;
        }

        void ws() {
            while (i < s.length() && Character.isWhitespace(s.charAt(i))) i++;
        }

        char peek() {
            if (i >= s.length()) throw new IllegalArgumentException("unexpected end");
            return s.charAt(i);
        }

        Object value() {
            ws();
            char c = peek();
            switch (c) {
                case '{': {
                    i++;
                    Map<String, Object> m = new LinkedHashMap<>();
                    ws();
                    if (peek() == '}') { i++; return m; }
                    while (true) {
                        ws();
                        if (peek() != '"') throw new IllegalArgumentException("expected a key at " + i);
                        String k = string();
                        ws();
                        if (peek() != ':') throw new IllegalArgumentException("expected : at " + i);
                        i++;
                        m.put(k, value());
                        ws();
                        char d = peek();
                        i++;
                        if (d == '}') return m;
                        if (d != ',') throw new IllegalArgumentException("expected , at " + (i - 1));
                    }
                }
                case '[': {
                    i++;
                    List<Object> l = new ArrayList<>();
                    ws();
                    if (peek() == ']') { i++; return l; }
                    while (true) {
                        l.add(value());
                        ws();
                        char d = peek();
                        i++;
                        if (d == ']') return l;
                        if (d != ',') throw new IllegalArgumentException("expected , at " + (i - 1));
                    }
                }
                case '"': return string();
                case 't': word("true"); return Boolean.TRUE;
                case 'f': word("false"); return Boolean.FALSE;
                case 'n': word("null"); return null;
                default: return number();
            }
        }

        void word(String w) {
            if (!s.startsWith(w, i)) throw new IllegalArgumentException("bad literal at " + i);
            i += w.length();
        }

        Object number() {
            int start = i;
            if (peek() == '-') i++;
            boolean frac = false;
            while (i < s.length()) {
                char c = s.charAt(i);
                if (c >= '0' && c <= '9') i++;
                else if (c == '.' || c == 'e' || c == 'E' || c == '+' || c == '-') { frac = true; i++; }
                else break;
            }
            String t = s.substring(start, i);
            if (t.isEmpty() || t.equals("-")) throw new IllegalArgumentException("bad value at " + start);
            if (frac) return new BigDecimal(t);
            BigInteger b = new BigInteger(t);
            return b.bitLength() < 64 ? (Object) b.longValue() : b;
        }

        String string() {
            i++; // opening quote
            StringBuilder b = new StringBuilder();
            while (true) {
                if (i >= s.length()) throw new IllegalArgumentException("unterminated string");
                char c = s.charAt(i++);
                if (c == '"') return b.toString();
                if (c != '\\') { b.append(c); continue; }
                char e = s.charAt(i++);
                switch (e) {
                    case 'n': b.append('\n'); break;
                    case 't': b.append('\t'); break;
                    case 'r': b.append('\r'); break;
                    case 'b': b.append('\b'); break;
                    case 'f': b.append('\f'); break;
                    case 'u': b.append((char) Integer.parseInt(s.substring(i, i + 4), 16)); i += 4; break;
                    default: b.append(e);
                }
            }
        }

        static String write(Object v) {
            StringBuilder b = new StringBuilder();
            write(b, v);
            return b.toString();
        }

        static void write(StringBuilder b, Object v) {
            if (v == null) {
                b.append("null");
            } else if (v instanceof String) {
                quote(b, (String) v);
            } else if (v instanceof Boolean) {
                b.append(v);
            } else if (v instanceof Double || v instanceof Float) {
                double d = ((Number) v).doubleValue();
                if (Double.isFinite(d)) b.append(d == Math.rint(d) && Math.abs(d) < 1e15 ? Long.toString((long) d) : Double.toString(d));
                else quote(b, Double.toString(d));
            } else if (v instanceof BigDecimal) {
                b.append(((BigDecimal) v).toPlainString());
            } else if (v instanceof Number) {
                b.append(v);
            } else if (v instanceof Map) {
                b.append('{');
                boolean first = true;
                for (Map.Entry<?, ?> e : ((Map<?, ?>) v).entrySet()) {
                    if (!first) b.append(',');
                    first = false;
                    quote(b, String.valueOf(e.getKey()));
                    b.append(':');
                    write(b, e.getValue());
                }
                b.append('}');
            } else if (v instanceof Collection) {
                b.append('[');
                boolean first = true;
                for (Object o : (Collection<?>) v) {
                    if (!first) b.append(',');
                    first = false;
                    write(b, o);
                }
                b.append(']');
            } else {
                quote(b, v.toString());
            }
        }

        static void quote(StringBuilder b, String s) {
            b.append('"');
            for (int k = 0; k < s.length(); k++) {
                char c = s.charAt(k);
                switch (c) {
                    case '"': b.append("\\\""); break;
                    case '\\': b.append("\\\\"); break;
                    case '\n': b.append("\\n"); break;
                    case '\r': b.append("\\r"); break;
                    case '\t': b.append("\\t"); break;
                    default:
                        if (c < 0x20 || c == 0x2028 || c == 0x2029) b.append(String.format("\\u%04x", (int) c));
                        else b.append(c);
                }
            }
            b.append('"');
        }
    }
}
