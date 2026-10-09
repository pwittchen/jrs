import java.io.BufferedOutputStream;
import java.io.BufferedReader;
import java.io.ByteArrayOutputStream;
import java.io.FileDescriptor;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStreamReader;
import java.io.OutputStream;
import java.io.PrintStream;
import java.nio.charset.Charset;
import java.nio.charset.StandardCharsets;
import javax.tools.JavaCompiler;
import javax.tools.ToolProvider;

/**
 * jrs's warm javac (specs/FASTER_BUILDS.md §6): one JVM for a `--watch`
 * session, compiling every request in process instead of starting javac
 * again.
 *
 * <p>Each request is one line on stdin, an id and the path of an argfile
 * jrs has written, as it would hand a forked javac. The answer on stdout is
 * a header line, {@code jrs-worker <id> <exit code> <stdout bytes> <stderr
 * bytes>}, followed by what javac wrote to each stream. javac runs with a
 * new context and a new file manager per call, closed after it, so no jar
 * stays open or cached from one build to the next.
 *
 * <p>The worker exits when stdin closes, which happens when jrs exits by any
 * route: it cannot outlive the command that started it.
 */
public final class JavacWorker {
    public static void main(String[] args) throws IOException {
        // The protocol owns the real stdout. An annotation processor printing
        // to System.out or System.err during a request writes into that
        // request's streams instead, as it would into a forked javac's.
        OutputStream protocol = new BufferedOutputStream(new FileOutputStream(FileDescriptor.out));
        Charset charset = Charset.forName(System.getProperty("native.encoding", "UTF-8"));
        JavaCompiler javac = ToolProvider.getSystemJavaCompiler();
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
                    code = javac.run(null, o, e, "@" + argfile);
                } catch (Throwable t) {
                    // javac's own abnormal-termination code.
                    t.printStackTrace(e);
                    code = 4;
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
