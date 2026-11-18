package opt

import "testing"

func TestEqual(t *testing.T) {
	one, alsoOne, two := 1, 1, 2
	for _, c := range []struct {
		a, b *int
		want bool
	}{
		{nil, nil, true},
		{&one, nil, false},
		{nil, &one, false},
		{&one, &alsoOne, true},
		{&one, &two, false},
	} {
		if got := Equal(c.a, c.b); got != c.want {
			t.Errorf("Equal(%v, %v) = %v", c.a, c.b, got)
		}
	}
}

func TestCloneIsACopy(t *testing.T) {
	if Clone[string](nil) != nil {
		t.Fatal("a nil clone is set")
	}
	s := "mixer"
	c := Clone(&s)
	if c == &s || *c != s {
		t.Fatalf("clone %p %q of %p %q", c, *c, &s, s)
	}
	s = "changed"
	if *c != "mixer" {
		t.Fatalf("the clone followed the original: %q", *c)
	}
}
