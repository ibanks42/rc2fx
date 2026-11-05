"""Separate observing faults from the final zero-fault qualification verdict."""
class ObservationPolicy:
    def __init__(self, keep_going=False):
        self.keep_going=keep_going
        self.observed_errors=set()
    def observe(self, errors, done):
        self.observed_errors.update(errors)
        stop=done or (bool(errors) and not self.keep_going)
        state='FAIL' if stop and self.observed_errors else ('PASS' if done else 'RUNNING')
        return {'state':state,'stop':stop,'qualificationFailed':bool(self.observed_errors),
                'observedErrors':sorted(self.observed_errors)}
