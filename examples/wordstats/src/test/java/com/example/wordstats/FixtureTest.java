package com.example.wordstats;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;

import com.example.wordstats.report.WordCount;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.util.List;
import org.junit.jupiter.api.Test;

/** Reads from src/test/resources, which must be on the test classpath but never in the jar. */
class FixtureTest {
    @Test
    void countsTheTestResource() throws Exception {
        String text;
        try (InputStream in = FixtureTest.class.getResourceAsStream("/fixture.txt")) {
            assertNotNull(in, "fixture.txt is not on the test classpath");
            text = new String(in.readAllBytes(), StandardCharsets.UTF_8);
        }

        WordCounter counter = WordCounter.withDefaultStopWords();
        counter.accept(text);

        assertEquals(List.of(new WordCount("jar", 5), new WordCount("zip", 2)), counter.top(10));
    }
}
