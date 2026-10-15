import java.io.BufferedOutputStream;
import java.io.BufferedReader;
import java.io.ByteArrayOutputStream;
import java.io.FileDescriptor;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStreamReader;
import java.io.OutputStream;
import java.io.PrintStream;
import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.nio.charset.Charset;
import java.nio.charset.StandardCharsets;

/**
 * jrs's warm kotlinc (specs/GRADLE_GAPS.md §3.3): one JVM for a
 * `--watch` session, compiling every request in process instead of starting
 * the Kotlin compiler again. JavacWorker's protocol, a second time.
 *
 * <p>The compiler's main class is the only argument, and its jars are on
 * this JVM's classpath. It is reached by reflection, through the
 * {@code exec(PrintStream, String...)} every Kotlin CLI tool has, so this
 * file compiles with a bare javac and serves any Kotlin version.
 *
 * <p>Each request is one line on stdin, an id and the path of the argfile
 * that holds the compiler's own arguments, the same file a forked run hands
 * it as {@code @argfile}. The answer on stdout is a header line,
 * {@code jrs-worker <id> <exit code> <stdout bytes> <stderr bytes>},
 * followed by what the compiler wrote to each stream. Each request gets a new
 * compiler instance, which disposes of its environment when it returns.
 *
 * <p>The worker exits when stdin closes, which happens when jrs exits by any
 * route: it cannot outlive the command that started it.
 */
public final class KotlincWorker {
    public static void main(String[] args) throws IOException, ReflectiveOperationException {
        // The protocol owns the real stdout. Whatever the compiler or a
        // plugin prints to System.out or System.err during a request goes
        // into that request's streams instead, as it would in a forked run.
        OutputStream protocol = new BufferedOutputStream(new FileOutputStream(FileDescriptor.out));
        Charset charset = Charset.forName(System.getProperty("native.encoding", "UTF-8"));
        Class<?> compiler = Class.forName(args[0]);
        Method exec = compiler.getMethod("exec", PrintStream.class, String[].class);
        BufferedReader requests =
                new BufferedReader(new InputStreamReader(System.in, StandardCharsets.UTF_8));
        String line;
        while ((line = requests.readLine()) != null) {
            int space = line.indexOf(' ');
            if (space < 0) {
                return;
            }
            String id = line.substring(0, space);
            String argfile = line.substring(space + 1);
            ByteArrayOutputStream out = new ByteArrayOutputStream();
            ByteArrayOutputStream err = new ByteArrayOutputStream();
            int code;
            PrintStream systemOut = System.out;
            PrintStream systemErr = System.err;
            try (PrintStream o = new PrintStream(out, true, charset);
                    PrintStream e = new PrintStream(err, true, charset)) {
                System.setOut(o);
                System.setErr(e);
                try {
                    Object instance = compiler.getDeclaredConstructor().newInstance();
                    Object exit = exec.invoke(instance, e, new String[] {"@" + argfile});
                    code = (Integer) exit.getClass().getMethod("getCode").invoke(exit);
                } catch (InvocationTargetException t) {
                    t.getCause().printStackTrace(e);
                    // The compiler's own INTERNAL_ERROR.
                    code = 2;
                } catch (Throwable t) {
                    t.printStackTrace(e);
                    code = 2;
                } finally {
                    System.setOut(systemOut);
                    System.setErr(systemErr);
                }
            }
            byte[] o = out.toByteArray();
            byte[] e = err.toByteArray();
            String header = "jrs-worker " + id + " " + code + " " + o.length + " " + e.length + "\n";
            protocol.write(header.getBytes(StandardCharsets.US_ASCII));
            protocol.write(o);
            protocol.write(e);
            protocol.flush();
        }
    }
}
