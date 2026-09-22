package com.example.bookmarks;

import static org.assertj.core.api.Assertions.assertThat;

import java.util.List;
import org.junit.jupiter.api.Test;

/** The store on its own: plain JUnit 5, no application context, milliseconds. */
class BookmarkStoreTest {

    private static BookmarkStore store(int maxSize, BookmarkProperties.Seed... seed) {
        return new BookmarkStore(new BookmarkProperties(maxSize, List.of(seed)));
    }

    @Test
    void seeds_are_stored_in_declaration_order() {
        BookmarkStore store = store(
                10,
                new BookmarkProperties.Seed("https://a.example", "A"),
                new BookmarkProperties.Seed("https://b.example", "B"));

        assertThat(store.all()).extracting(Bookmark::title).containsExactly("A", "B");
        assertThat(store.all()).extracting(Bookmark::id).containsExactly(1L, 2L);
    }

    @Test
    void an_added_bookmark_is_found_by_its_id() {
        BookmarkStore store = store(10);

        Bookmark added = store.add("https://getjrs.dev", "jrs").orElseThrow();

        assertThat(store.find(added.id())).contains(added);
        assertThat(store.find(added.id() + 1)).isEmpty();
    }

    @Test
    void the_store_refuses_to_grow_past_its_limit() {
        BookmarkStore store = store(1);

        assertThat(store.add("https://a.example", "A")).isPresent();
        assertThat(store.add("https://b.example", "B")).isEmpty();
        assertThat(store.all()).hasSize(1);
    }

    @Test
    void removing_reports_whether_anything_was_there() {
        BookmarkStore store = store(10);
        Bookmark added = store.add("https://a.example", "A").orElseThrow();

        assertThat(store.remove(added.id())).isTrue();
        assertThat(store.remove(added.id())).isFalse();
        assertThat(store.all()).isEmpty();
    }
}
