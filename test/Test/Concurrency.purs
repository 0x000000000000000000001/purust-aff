module Test.Concurrency where

import Prelude
import Data.Array as Array
import Data.Either (Either(..))
import Data.Traversable (traverse, traverse_)
import Effect (Effect, forE)
import Effect.Aff (Aff, launchAff_, makeAff, forkAff, joinFiber, never, supervise)
import Effect.Aff.AVar as AVar
import Effect.Class (liftEffect)
import Effect.Console (log)
import Effect.Ref as Ref
import Test.Assert (assertEqual)
import Test.Stress as Stress

foreign import data Rendezvous :: Type
foreign import rendezvous :: Effect Rendezvous
foreign import arrive :: Rendezvous -> Effect Unit
foreign import threadCount :: Rendezvous -> Effect Int
foreign import enqueue :: Effect Unit -> Effect Unit

-- An external callback may complete from any runtime worker. Timers have their
-- own deterministic ordering, tested by the original Aff suite.
workerYield :: Aff Unit
workerYield = makeAff \done -> do
  enqueue (done (Right unit))
  pure mempty

main :: Effect Unit
main = launchAff_ do
  meeting <- liftEffect rendezvous
  let visit = workerYield *> liftEffect (arrive meeting)
  first <- forkAff visit
  second <- forkAff visit
  joinFiber first
  joinFiber second
  count <- liftEffect $ threadCount meeting
  liftEffect do
    assertEqual { actual: count, expected: 2 }
    log "[OK] Aff resumes on distinct Tokio worker threads"

  ref <- liftEffect $ Ref.new 0
  workers <- traverse forkAff $ Array.replicate 8 do
    workerYield
    liftEffect $ forE 0 1000 \_ -> Ref.modify_ (_ + 1) ref
  traverse_ joinFiber workers
  total <- liftEffect $ Ref.read ref
  liftEffect do
    assertEqual { actual: total, expected: 8000 }
    log "[OK] Aff shared Ref: 8000 atomic modifications"

  Stress.stressAVar
  liftEffect $ log "[OK] original Go AVar stress: 1000 items"

  queue <- AVar.empty
  consumed <- liftEffect $ Ref.new 0
  consumers <- traverse forkAff $ Array.replicate 1000 do
    workerYield
    value <- AVar.take queue
    liftEffect $ Ref.modify_ (_ + value) consumed
  producers <- traverse forkAff $ Array.replicate 1000 do
    workerYield
    AVar.put 1 queue
  traverse_ joinFiber consumers
  traverse_ joinFiber producers
  sum <- liftEffect $ Ref.read consumed
  liftEffect do
    assertEqual { actual: sum, expected: 1000 }
    log "[OK] Aff/AVar: 1000 producers and consumers across suspensions"

  supervise do
    void $ forkAff (never :: Aff Unit)
    pure unit
  liftEffect $ log "[OK] supervise cancels an unreferenced never fiber"
