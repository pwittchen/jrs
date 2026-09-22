package com.example.bookmarks;

import java.net.URI;
import java.util.List;
import org.springframework.http.ResponseEntity;
import org.springframework.web.bind.annotation.DeleteMapping;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.PathVariable;
import org.springframework.web.bind.annotation.PostMapping;
import org.springframework.web.bind.annotation.RequestBody;
import org.springframework.web.bind.annotation.RequestMapping;
import org.springframework.web.bind.annotation.RestController;

/** The REST API: list, read, create, delete. */
@RestController
@RequestMapping("/api/bookmarks")
public class BookmarkController {

    private final BookmarkStore store;

    public BookmarkController(BookmarkStore store) {
        this.store = store;
    }

    @GetMapping
    public List<Bookmark> list() {
        return store.all();
    }

    @GetMapping("/{id}")
    public ResponseEntity<Bookmark> get(@PathVariable long id) {
        return ResponseEntity.of(store.find(id));
    }

    @PostMapping
    public ResponseEntity<Bookmark> create(@RequestBody NewBookmark request) {
        return store.add(request.url(), request.title())
                .map(bookmark ->
                        ResponseEntity.created(URI.create("/api/bookmarks/" + bookmark.id()))
                                .body(bookmark))
                .orElseGet(() -> ResponseEntity.status(409).build());
    }

    @DeleteMapping("/{id}")
    public ResponseEntity<Void> delete(@PathVariable long id) {
        return store.remove(id) ? ResponseEntity.noContent().build() : ResponseEntity.notFound().build();
    }

    /** What a POST carries: the id is the store's to hand out. */
    public record NewBookmark(String url, String title) {}
}
